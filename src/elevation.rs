use crate::lcu::{self, Reply, Request};
use interprocess::local_socket::{
    GenericNamespaced, ListenerNonblockingMode, ListenerOptions, prelude::*,
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

pub struct Connection {
    stream: interprocess::local_socket::Stream,
    process: HANDLE,
}

impl Drop for Connection {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.process) };
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
        Ok(Connection {
            stream,
            process: info.hProcess,
        })
    })();
    if result.is_err() {
        unsafe { CloseHandle(info.hProcess) };
    }
    result.map_err(str::to_owned)
}

impl Connection {
    pub fn query(&mut self, request: Request) -> Result<Reply, &'static str> {
        let mut command = [0; 37];
        if let Request::Detail(id) = request {
            command[0] = 1;
            command[1..9].copy_from_slice(&id.to_le_bytes());
        }
        if let Request::Profile(puuid) = request {
            command[0] = 2;
            command[1..37].copy_from_slice(&puuid);
        }
        self.stream
            .write_all(&command)
            .map_err(|_| "同步连接已断开。请点击「连接并同步」重新连接。")?;
        let deadline = Instant::now() + Duration::from_secs(45);
        let mut data = Vec::new();
        let mut buffer = [0u8; 8192];
        loop {
            let worker_exited = unsafe { WaitForSingleObject(self.process, 0) } == 0;
            match self.stream.read(&mut buffer) {
                // Windows may report an empty read while the worker is still querying LCU.
                Ok(0) => {
                    if worker_exited {
                        return Err("管理员同步进程退出前未返回完整结果。");
                    }
                    thread::sleep(Duration::from_millis(50));
                }
                Ok(n) => {
                    data.extend_from_slice(&buffer[..n]);
                    if data.len() > 2 * 1024 * 1024 {
                        return Err("同步结果超过大小限制。");
                    }
                    if data.ends_with(b"\n") {
                        break;
                    }
                }
                Err(error)
                    if error.kind() == ErrorKind::WouldBlock
                        || error.raw_os_error() == Some(232) =>
                {
                    if worker_exited {
                        return Err("管理员同步进程退出前未返回完整结果。");
                    }
                    thread::sleep(Duration::from_millis(50))
                }
                Err(_) => return Err("同步通道读取失败。"),
            }
            if Instant::now() >= deadline {
                return Err("管理员同步超时。");
            }
        }
        serde_json::from_slice(&data).map_err(|_| "管理员同步结果无效。")
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

    let mut command = [0; 37];
    while stream.read_exact(&mut command).is_ok() {
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
            _ => break,
        };
        let Ok(mut data) = serde_json::to_vec(&lcu::run(request)) else {
            break;
        };
        data.push(b'\n');
        if stream.write_all(&data).is_err() {
            break;
        }
    }
}
