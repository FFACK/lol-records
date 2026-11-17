use crate::lcu::{self, Reply, Request};
use interprocess::local_socket::{
    GenericNamespaced, ListenerNonblockingMode, ListenerOptions, prelude::*,
};
use std::{
    ffi::OsStr,
    fs::OpenOptions,
    io::{ErrorKind, Read, Write},
    os::windows::{ffi::OsStrExt, fs::OpenOptionsExt},
    thread,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::CloseHandle,
    System::Threading::{GetProcessId, WaitForSingleObject},
    UI::{
        Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW},
        WindowsAndMessaging::SW_HIDE,
    },
};

fn wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(Some(0)).collect()
}

pub fn elevated(request: Request) -> Result<Reply, String> {
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
    let command = match request {
        Request::Sync => "sync".into(),
        Request::Detail(id) => id.to_string(),
    };
    let args = wide(OsStr::new(&format!("--lcu-worker {pipe} {command}")));
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
        let mut stream = loop {
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
        let mut data = Vec::new();
        let mut buffer = [0u8; 8192];
        loop {
            match stream.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    data.extend_from_slice(&buffer[..n]);
                    if data.len() > 2 * 1024 * 1024 {
                        return Err("同步结果超过大小限制。");
                    }
                    if data.ends_with(b"\n") {
                        break;
                    }
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(50))
                }
                Err(_) => return Err("同步通道读取失败。"),
            }
            if Instant::now() >= deadline {
                return Err("管理员同步超时。");
            }
        }
        serde_json::from_slice(&data).map_err(|_| "管理员同步结果无效。")
    })();
    unsafe { CloseHandle(info.hProcess) };
    result.map_err(str::to_owned)
}

pub fn worker(pipe: &str, command: &str) {
    let Some(nonce) = pipe.strip_prefix("lol-records-") else {
        return;
    };
    if nonce.len() != 32 || !nonce.bytes().all(|b| b.is_ascii_hexdigit()) {
        return;
    }
    let request = if command == "sync" {
        Request::Sync
    } else {
        match command.parse() {
            Ok(id) => Request::Detail(id),
            Err(_) => return,
        }
    };
    // Identification-level SQOS prevents a pipe server from impersonating this process.
    let Ok(mut stream) = OpenOptions::new()
        .write(true)
        .security_qos_flags(0x0001_0000)
        .open(format!(r"\\.\pipe\{pipe}"))
    else {
        return;
    };
    if let Ok(mut data) = serde_json::to_vec(&lcu::run(request)) {
        data.push(b'\n');
        let _ = stream.write_all(&data);
    }
}
