//! Separate foreground and background lanes, sharing a single authenticated connection.
use crate::{Reply, Request, Ticket, elevation, lcu};
use native_windows_gui as nwg;
use std::sync::{Arc, Mutex, mpsc};

type Job = (Request, Option<Ticket>);

pub(crate) fn workers<T: Send + 'static>(
    count: usize,
    receiver: mpsc::Receiver<T>,
    process: impl Fn(T) + Send + Sync + 'static,
) -> Vec<std::thread::JoinHandle<()>> {
    let receiver = Arc::new(Mutex::new(receiver));
    let process = Arc::new(process);
    (0..count)
        .map(|_| {
            let receiver = receiver.clone();
            let process = process.clone();
            std::thread::spawn(move || {
                loop {
                    let job = receiver.lock().ok().and_then(|rx| rx.recv().ok());
                    let Some(job) = job else { break };
                    process(job);
                }
            })
        })
        .collect()
}

#[derive(Clone)]
enum Client {
    Direct(Arc<lcu::Session>),
    Elevated(elevation::Connection),
}
impl Client {
    fn run(&self, request: Request) -> Reply {
        match self {
            Self::Direct(session) => session.run(request),
            Self::Elevated(connection) => connection
                .query(request)
                .unwrap_or_else(|e| Reply::Error(e.into())),
        }
    }
}

pub(crate) fn run(
    jobs: mpsc::Receiver<Job>,
    tx: mpsc::Sender<(Option<Ticket>, Reply)>,
    notice: nwg::NoticeSender,
) {
    let state: Arc<Mutex<Option<Client>>> = Arc::new(Mutex::new(None));
    let (foreground_tx, foreground_rx) = mpsc::channel();
    let (ranks_tx, ranks_rx) = mpsc::channel();
    for (count, receiver) in [(1, foreground_rx), (3, ranks_rx)] {
        let state = state.clone();
        let tx = tx.clone();
        workers(count, receiver, move |(request, ticket): Job| {
            let client = {
                let mut state = state.lock().unwrap();
                if matches!(request, Request::Sync)
                    && state.as_ref().is_some_and(|client| match client {
                        Client::Direct(_) => true,
                        Client::Elevated(connection) => connection.is_closed(),
                    })
                {
                    *state = None;
                }
                if state.is_none() && matches!(request, Request::Sync) {
                    let connected = match lcu::Session::connect() {
                        Ok(session) => Ok(Client::Direct(Arc::new(session))),
                        Err(Reply::AccessDenied) => elevation::connect()
                            .map(Client::Elevated)
                            .map_err(Reply::Error),
                        Err(error) => Err(error),
                    };
                    match connected {
                        Ok(client) => *state = Some(client),
                        Err(error) => {
                            let _ = tx.send((ticket, error));
                            notice.notice();
                            return;
                        }
                    }
                }
                state.clone()
            };
            let reply = client
                .as_ref()
                .map(|c| c.run(request))
                .unwrap_or_else(|| Reply::Error("请点击「刷新战绩」建立连接。".into()));
            let reply = if let Request::Rank(bytes) = request {
                if matches!(reply, Reply::Rank { .. }) {
                    reply
                } else {
                    Reply::Rank {
                        puuid: String::from_utf8_lossy(&bytes).into_owned(),
                        rank: None,
                    }
                }
            } else {
                reply
            };
            // Refresh can establish a new session after a transport/client restart.
            if matches!(request, Request::Sync)
                && client.as_ref().is_some_and(
                    |c| matches!(c,Client::Elevated(connection) if connection.is_closed()),
                )
            {
                *state.lock().unwrap() = None;
            }
            if tx.send((ticket, reply)).is_ok() {
                notice.notice();
            }
        });
    }
    for job in jobs {
        let lane = if matches!(job.0, Request::Rank(_)) {
            &ranks_tx
        } else {
            &foreground_tx
        };
        if lane.send(job).is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rank_workers_overlap_and_leave_foreground_capacity() {
        let (background_tx, background_rx) = mpsc::channel();
        let (started_tx, started_rx) = mpsc::channel();
        let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let gate_worker = gate.clone();
        let background = workers(3, background_rx, move |_: u8| {
            started_tx.send(()).unwrap();
            let (lock, ready) = &*gate_worker;
            let mut open = lock.lock().unwrap();
            while !*open {
                open = ready.wait(open).unwrap();
            }
        });
        for id in 0..3 {
            background_tx.send(id).unwrap();
        }
        let deadline = std::time::Duration::from_secs(2);
        let overlapping = (0..3).all(|_| started_rx.recv_timeout(deadline).is_ok());
        let (front_tx, front_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let foreground = workers(1, front_rx, move |_: u8| {
            done_tx.send(()).unwrap();
        });
        front_tx.send(1).unwrap();
        let front_completed = done_rx.recv_timeout(deadline).is_ok();
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        drop(background_tx);
        drop(front_tx);
        for worker in background.into_iter().chain(foreground) {
            worker.join().unwrap();
        }
        assert!(
            overlapping,
            "all three rank requests must run before the gate opens"
        );
        assert!(
            front_completed,
            "foreground must finish while rank requests are blocked"
        );
    }
}
