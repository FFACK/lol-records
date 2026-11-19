use crate::lcu::{self, Reply, Request};
use interprocess::TryClone;
use interprocess::local_socket::{
    GenericNamespaced, ListenerNonblockingMode, ListenerOptions, prelude::*,
};
use std::collections::HashMap;
use std::sync::{
    Arc, Mutex, Weak,
    atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc,
};
use std::{
    ffi::OsStr,
    fs::OpenOptions,
    io::{ErrorKind, Read, Write},
    os::windows::{ffi::OsStrExt, fs::OpenOptionsExt, io::AsRawHandle},
    thread,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE},
    System::{
        Pipes::GetNamedPipeServerProcessId,
        Threading::{GetProcessId, WaitForSingleObject},
    },
    UI::{
        Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW},
        WindowsAndMessaging::SW_HIDE,
    },
};

fn wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(Some(0)).collect()
}

#[derive(Clone)]
pub struct Connection {
    inner: Arc<Inner>,
}
struct Inner {
    stream: Mutex<interprocess::local_socket::Stream>,
    process: usize,
    next: AtomicU64,
    closed: AtomicBool,
    pending: Mutex<HashMap<u64, mpsc::Sender<Result<Reply, &'static str>>>>,
}
impl Drop for Inner {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.process as HANDLE) };
    }
}
#[derive(serde::Serialize, serde::Deserialize)]
struct Response {
    id: u64,
    reply: Reply,
}
fn fail(inner: &Inner) {
    inner.closed.store(true, Ordering::Release);
    for (_, sender) in inner.pending.lock().unwrap().drain() {
        let _ = sender.send(Err("同步连接已断开，请点击「刷新战绩」重新连接。"));
    }
}
fn deliver(
    pending: &mut HashMap<u64, mpsc::Sender<Result<Reply, &'static str>>>,
    response: Response,
) {
    if let Some(sender) = pending.remove(&response.id) {
        let _ = sender.send(Ok(response.reply));
    }
}
fn read_responses(mut stream: interprocess::local_socket::Stream, weak: Weak<Inner>) {
    let mut data = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        let Some(inner) = weak.upgrade() else {
            break;
        };
        if inner.closed.load(Ordering::Acquire) {
            break;
        }
        match stream.read(&mut buffer) {
            Ok(0) => {
                if unsafe { WaitForSingleObject(inner.process as HANDLE, 0) } == 0 {
                    fail(&inner);
                    break;
                }
                thread::sleep(Duration::from_millis(5));
            }
            Ok(n) => {
                data.extend_from_slice(&buffer[..n]);
                if data.len() > 2 * 1024 * 1024 {
                    fail(&inner);
                    break;
                }
                while let Some(end) = data.iter().position(|b| *b == b'\n') {
                    let parsed = serde_json::from_slice::<Response>(&data[..end]);
                    data.drain(..=end);
                    match parsed {
                        Ok(response) => deliver(&mut inner.pending.lock().unwrap(), response),
                        Err(_) => {
                            fail(&inner);
                            return;
                        }
                    }
                }
            }
            Err(error)
                if error.kind() == ErrorKind::WouldBlock || error.raw_os_error() == Some(232) =>
            {
                thread::sleep(Duration::from_millis(5))
            }
            Err(_) => {
                fail(&inner);
                break;
            }
        }
    }
}

pub fn connect() -> Result<Connection, String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| "无法创建本地同步会话。")?;
    let nonce = u128::from_ne_bytes(bytes);
    let pipe = format!("lol-records-{nonce:032x}");
    let name = pipe
        .as_str()
        .to_ns_name::<GenericNamespaced>()
        .map_err(|_| "无法创建本地同步通道。")?;
    let listener = ListenerOptions::new()
        .name(name)
        .nonblocking(ListenerNonblockingMode::Both)
        .create_sync()
        .map_err(|_| "无法监听本地同步通道。")?;
    let args = wide(OsStr::new(&format!(
        "--lcu-worker {pipe} {}",
        std::process::id()
    )));
    let executable = std::env::current_exe().map_err(|_| "无法定位当前程序。")?;
    let executable = wide(executable.as_os_str());
    let verb = wide(OsStr::new("runas"));
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS;
    info.lpVerb = verb.as_ptr();
    info.lpFile = executable.as_ptr();
    info.lpParameters = args.as_ptr();
    info.nShow = SW_HIDE;
    if unsafe { ShellExecuteExW(&mut info) } == 0 {
        return Err(
            if std::io::Error::last_os_error().raw_os_error() == Some(1223) {
                "已取消权限请求。".into()
            } else {
                "无法启动管理员同步进程。".into()
            },
        );
    }
    let result = (|| {
        let deadline = Instant::now() + Duration::from_secs(45);
        let stream = loop {
            match listener.accept() {
                Ok(stream) => break stream,
                Err(error) if error.kind() == ErrorKind::WouldBlock => (),
                Err(_) => return Err("同步通道连接失败。"),
            }
            if Instant::now() >= deadline || unsafe { WaitForSingleObject(info.hProcess, 0) } == 0 {
                return Err("管理员同步进程未连接。");
            }
            thread::sleep(Duration::from_millis(50));
        };
        let expected_pid = unsafe { GetProcessId(info.hProcess) };
        if expected_pid == 0
            || stream
                .peer_creds()
                .ok()
                .and_then(|credentials| credentials.pid())
                != Some(expected_pid)
        {
            return Err("本地同步进程身份不匹配。");
        }
        Connection::new(stream, info.hProcess)
    })();
    if result.is_err() {
        unsafe { CloseHandle(info.hProcess) };
    }
    result.map_err(str::to_owned)
}

fn encode(request: Request, id: u64) -> Result<[u8; 137], &'static str> {
    let mut command = [0; 129];
    if let Request::Detail(id) = request {
        command[0] = 1;
        command[1..9].copy_from_slice(&id.to_le_bytes());
    }
    if let Request::Profile(puuid) = request {
        command[0] = 2;
        command[1..37].copy_from_slice(&puuid);
    }
    if let Request::Search(bytes) = request {
        if lcu::search_text(&bytes).is_none() {
            return Err("搜索名称无效。");
        }
        command[0] = 3;
        command[1..129].copy_from_slice(&bytes);
    }
    if matches!(request, Request::Live) {
        command[0] = 4;
    }
    if let Request::Rank(bytes) = request {
        command[0] = 5;
        command[1..37].copy_from_slice(&bytes);
    }
    let mut frame = [0; 137];
    frame[..8].copy_from_slice(&id.to_le_bytes());
    frame[8..].copy_from_slice(&command);
    Ok(frame)
}
impl Connection {
    fn new(
        stream: interprocess::local_socket::Stream,
        process: HANDLE,
    ) -> Result<Self, &'static str> {
        let reader = stream.try_clone().map_err(|_| "无法创建并发同步连接。")?;
        let inner = Arc::new(Inner {
            stream: Mutex::new(stream),
            process: process as usize,
            next: AtomicU64::new(1),
            closed: AtomicBool::new(false),
            pending: Mutex::new(HashMap::new()),
        });
        let weak = Arc::downgrade(&inner);
        thread::spawn(move || read_responses(reader, weak));
        Ok(Self { inner })
    }
    pub fn is_closed(&self) -> bool {
        self.inner.closed.load(Ordering::Acquire)
    }
    pub fn query(&self, request: Request) -> Result<Reply, &'static str> {
        if self.is_closed() {
            return Err("同步连接已断开，请点击「刷新战绩」重新连接。");
        }
        let id = self.inner.next.fetch_add(1, Ordering::Relaxed);
        let frame = encode(request, id)?;
        let (tx, rx) = mpsc::channel();
        {
            let mut pending = self.inner.pending.lock().unwrap();
            if self.is_closed() {
                return Err("同步连接已断开，请点击「刷新战绩」重新连接。");
            }
            pending.insert(id, tx);
        }
        if self.inner.stream.lock().unwrap().write_all(&frame).is_err() {
            fail(&self.inner);
            return Err("同步通道写入失败。");
        }
        let result = rx
            .recv_timeout(Duration::from_secs(45))
            .map_err(|_| "同步请求超时，请重试。");
        self.inner.pending.lock().unwrap().remove(&id);
        result?
    }
}

pub fn worker(pipe: &str, parent_pid: u32) {
    let Some(nonce) = pipe.strip_prefix("lol-records-") else {
        return;
    };
    if nonce.len() != 32 || !nonce.bytes().all(|b| b.is_ascii_hexdigit()) {
        return;
    }

    // The elevated process must not grant its impersonation token to the pipe server.
    let Ok(mut stream) = OpenOptions::new()
        .read(true)
        .write(true)
        .security_qos_flags(0x0001_0000)
        .open(format!(r"\\.\pipe\{pipe}"))
    else {
        return;
    };
    let mut server_pid = 0;
    if unsafe { GetNamedPipeServerProcessId(stream.as_raw_handle(), &mut server_pid) } == 0
        || server_pid != parent_pid
    {
        return;
    }

    let Ok(writer) = stream.try_clone() else {
        return;
    };
    let writer = Arc::new(Mutex::new(writer));
    let session: Arc<Mutex<Option<Arc<lcu::Session>>>> = Arc::new(Mutex::new(None));
    let (front_tx, front_rx) = mpsc::sync_channel(16);
    let (rank_tx, rank_rx) = mpsc::sync_channel(16);
    for (count, receiver) in [(1, front_rx), (3, rank_rx)] {
        let writer = writer.clone();
        let session = session.clone();
        crate::query::workers(count, receiver, move |(id, request): (u64, Request)| {
            let active = {
                let mut cached = session.lock().unwrap();
                if matches!(request, Request::Sync) {
                    *cached = None;
                }
                if cached.is_none() {
                    match lcu::Session::connect() {
                        Ok(active) => *cached = Some(Arc::new(active)),
                        Err(error) => {
                            send_response(&writer, id, error);
                            return;
                        }
                    }
                }
                cached.as_ref().unwrap().clone()
            };
            send_response(&writer, id, active.run(request));
        });
    }
    let mut frame = [0; 137];
    while stream.read_exact(&mut frame).is_ok() {
        let id = u64::from_le_bytes(frame[..8].try_into().unwrap());
        let command = &frame[8..];
        let request = match command[0] {
            0 => Request::Sync,
            1 => Request::Detail(u64::from_le_bytes(command[1..9].try_into().unwrap())),
            2 => {
                let bytes: [u8; 36] = command[1..37].try_into().unwrap();
                if !std::str::from_utf8(&bytes).is_ok_and(lcu::valid_puuid) {
                    break;
                }
                Request::Profile(bytes)
            }
            3 => {
                let bytes: [u8; 128] = command[1..129].try_into().unwrap();
                if lcu::search_text(&bytes).is_none() {
                    break;
                }
                Request::Search(bytes)
            }
            4 => Request::Live,
            5 => {
                let bytes: [u8; 36] = command[1..37].try_into().unwrap();
                if !std::str::from_utf8(&bytes).is_ok_and(lcu::valid_puuid) {
                    break;
                }
                Request::Rank(bytes)
            }
            _ => break,
        };
        let lane = if matches!(request, Request::Rank(_)) {
            &rank_tx
        } else {
            &front_tx
        };
        if lane.try_send((id, request)).is_err() {
            send_response(&writer, id, Reply::Error("请求过多，请稍后重试。".into()));
        }
    }
}
fn send_response(writer: &Mutex<std::fs::File>, id: u64, reply: Reply) {
    if let Ok(mut data) = serde_json::to_vec(&Response { id, reply }) {
        data.push(b'\n');
        let _ = writer.lock().unwrap().write_all(&data);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn concurrent_pipe_requests_receive_out_of_order_replies_correctly() {
        let mut random = [0; 16];
        getrandom::fill(&mut random).unwrap();
        let pipe = format!("lol-records-test-{:032x}", u128::from_le_bytes(random));
        let listener = ListenerOptions::new()
            .name(pipe.as_str().to_ns_name::<GenericNamespaced>().unwrap())
            .nonblocking(ListenerNonblockingMode::Both)
            .create_sync()
            .unwrap();
        let server = thread::spawn(move || {
            let mut file = OpenOptions::new()
                .read(true)
                .write(true)
                .open(format!(r"\\.\pipe\{pipe}"))
                .unwrap();
            let mut frames = [[0; 137]; 2];
            for frame in &mut frames {
                file.read_exact(frame).unwrap();
            }
            let writer = Mutex::new(file);
            for frame in frames.into_iter().rev() {
                let id = u64::from_le_bytes(frame[..8].try_into().unwrap());
                let game = u64::from_le_bytes(frame[9..17].try_into().unwrap());
                send_response(&writer, id, Reply::Error(game.to_string()));
            }
        });
        let deadline = Instant::now() + Duration::from_secs(3);
        let stream = loop {
            match listener.accept() {
                Ok(stream) => break stream,
                Err(e) if e.kind() == ErrorKind::WouldBlock && Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(1))
                }
                Err(e) => panic!("test pipe accept failed: {e}"),
            }
        };
        let process = unsafe {
            windows_sys::Win32::System::Threading::OpenProcess(0x00100000, 0, std::process::id())
        };
        assert!(!process.is_null());
        let connection = Connection::new(stream, process).unwrap();
        thread::scope(|scope| {
            let a = scope.spawn(|| connection.query(Request::Detail(10)));
            let b = scope.spawn(|| connection.query(Request::Detail(20)));
            assert!(matches!(a.join().unwrap(),Ok(Reply::Error(s)) if s=="10"));
            assert!(matches!(b.join().unwrap(),Ok(Reply::Error(s)) if s=="20"));
        });
        server.join().unwrap();
    }

    #[test]
    fn reversed_responses_are_matched_to_their_own_request() {
        let (a, ar) = mpsc::channel();
        let (b, br) = mpsc::channel();
        let mut pending = HashMap::from([(1, a), (2, b)]);
        deliver(
            &mut pending,
            Response {
                id: 2,
                reply: Reply::Error("second".into()),
            },
        );
        assert!(ar.try_recv().is_err());
        assert!(matches!(br.recv().unwrap(),Ok(Reply::Error(s)) if s=="second"));
        deliver(
            &mut pending,
            Response {
                id: 1,
                reply: Reply::Error("first".into()),
            },
        );
        assert!(matches!(ar.recv().unwrap(),Ok(Reply::Error(s)) if s=="first"));
        assert!(pending.is_empty());
    }
    #[test]
    fn frame_keeps_request_identifier_separate_from_validated_parameters() {
        let frame = encode(Request::Detail(987), 42).unwrap();
        assert_eq!(u64::from_le_bytes(frame[..8].try_into().unwrap()), 42);
        assert_eq!(frame[8], 1);
        assert_eq!(u64::from_le_bytes(frame[9..17].try_into().unwrap()), 987);
        assert!(encode(Request::Search([0; 128]), 1).is_err());
    }
}
