use base64::{Engine, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signature, VerifyingKey};
use semver::Version;
use serde::Deserialize;
use std::{
    ffi::OsString,
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Child, Command},
    thread,
    time::{Duration, Instant},
};
use ureq::{
    Agent,
    tls::{RootCerts, TlsConfig, TlsProvider},
};
use windows_sys::Win32::{
    Foundation::CloseHandle,
    System::Threading::{OpenProcess, WaitForSingleObject},
};

const PACKAGE: &str = "lol-records.exe";
const SIGNATURE: &str = "lol-records.exe.sig";
const LIMIT: u64 = 16 * 1024 * 1024;
const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone)]
pub struct Release {
    pub tag: String,
    repo: String,
}

#[derive(Deserialize)]
struct Metadata {
    tag_name: String,
    prerelease: bool,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    assets: Vec<Asset>,
}
#[derive(Deserialize)]
struct Asset {
    name: String,
}

fn agent() -> Agent {
    Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(20)))
        .tls_config(
            TlsConfig::builder()
                .provider(TlsProvider::NativeTls)
                .root_certs(RootCerts::PlatformVerifier)
                .build(),
        )
        .build()
        .new_agent()
}

fn config() -> Option<(&'static str, VerifyingKey)> {
    let repo = option_env!("LOL_RECORDS_UPDATE_REPO")?;
    let (owner, name) = repo.split_once('/')?;
    let valid = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    };
    if !valid(owner) || !valid(name) {
        return None;
    }
    let bytes: [u8; 32] = STANDARD
        .decode(option_env!("LOL_RECORDS_UPDATE_PUBKEY")?)
        .ok()?
        .try_into()
        .ok()?;
    Some((repo, VerifyingKey::from_bytes(&bytes).ok()?))
}

pub fn check() -> Option<Release> {
    let (repo, _) = config()?;
    let metadata: Metadata = agent()
        .get(format!(
            "https://gitee.com/api/v5/repos/{repo}/releases/latest"
        ))
        .header("User-Agent", "lol-records")
        .call()
        .ok()?
        .body_mut()
        .read_json()
        .ok()?;
    let version = Version::parse(metadata.tag_name.strip_prefix('v')?).ok()?;
    if metadata.draft
        || metadata.prerelease
        || !version.pre.is_empty()
        || metadata.tag_name != format!("v{version}")
        || version <= Version::parse(VERSION).ok()?
        || ![PACKAGE, SIGNATURE]
            .iter()
            .all(|name| metadata.assets.iter().any(|asset| asset.name == *name))
    {
        return None;
    }
    Some(Release {
        tag: metadata.tag_name,
        repo: repo.to_owned(),
    })
}

fn fetch(
    agent: &Agent,
    release: &Release,
    asset: &str,
    limit: u64,
) -> Result<Vec<u8>, &'static str> {
    let mut response = agent
        .get(format!(
            "https://gitee.com/{}/releases/download/{}/{asset}",
            release.repo, release.tag
        ))
        .header("User-Agent", "lol-records")
        .call()
        .map_err(|_| "Gitee 下载失败。")?;
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "更新包下载中断。")?;
    if bytes.len() as u64 > limit {
        return Err("更新包超过大小限制。");
    }
    Ok(bytes)
}

fn verify(tag: &str, bytes: &[u8], signature: &[u8]) -> Result<(), &'static str> {
    let (_, key) = config().ok_or("缺少更新公钥。")?;
    let signature = Signature::from_slice(signature).map_err(|_| "更新签名格式无效。")?;
    let mut message = format!("lol-records {tag} windows-x64\n").into_bytes();
    message.extend_from_slice(bytes);
    key.verify_strict(&message, &signature)
        .map_err(|_| "更新包验签失败。")
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name: OsString = path.as_os_str().into();
    name.push(suffix);
    PathBuf::from(name)
}

pub fn stage(release: &Release) -> Result<(), &'static str> {
    let agent = agent();
    let signature = fetch(&agent, release, SIGNATURE, 64)?;
    let binary = fetch(&agent, release, PACKAGE, LIMIT)?;
    verify(&release.tag, &binary, &signature)?;
    let current = std::env::current_exe().map_err(|_| "无法定位当前程序。")?;
    let staged = sibling(&current, ".new");
    let sig = sibling(&current, ".new.sig");
    let helper = sibling(&current, ".update.exe");
    let old = fs::read(&current).map_err(|_| "无法读取当前程序。")?;
    let mut created = Vec::new();
    for (path, data) in [
        (&staged, binary.as_slice()),
        (&sig, signature.as_slice()),
        (&helper, old.as_slice()),
    ] {
        if write_new(path, data).is_err() {
            for file in created {
                let _ = fs::remove_file(file);
            }
            return Err("更新所需文件已存在，或当前目录不可写。");
        }
        created.push(path);
    }
    let result = Command::new(&helper)
        .arg("--install-update")
        .arg(std::process::id().to_string())
        .arg(&release.tag)
        .spawn();
    if result.is_err() {
        for path in [staged, sig, helper] {
            let _ = fs::remove_file(path);
        }
        return Err("无法启动临时更新进程。");
    }
    Ok(())
}

fn write_new(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    let result = file.write_all(data).and_then(|_| file.sync_all());
    if result.is_err() {
        let _ = fs::remove_file(path);
    }
    result
}

pub fn install(pid: u32, tag: &str) {
    if let Err(message) = apply(pid, tag) {
        if let Some(original) = update_target() {
            let _ = fs::remove_file(sibling(&original, ".new"));
            let _ = fs::remove_file(sibling(&original, ".new.sig"));
        }
        native_windows_gui::simple_message("更新失败", message);
    }
}

fn update_target() -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    executable
        .to_str()?
        .strip_suffix(".update.exe")
        .map(PathBuf::from)
}

fn apply(pid: u32, tag: &str) -> Result<(), &'static str> {
    let target = update_target().ok_or("更新进程路径不正确。")?;
    let path = target.as_path();
    let staged = sibling(path, ".new");
    let sig = sibling(path, ".new.sig");
    let backup = sibling(path, ".old");
    let ready = sibling(path, ".ready");
    let binary = fs::read(&staged).map_err(|_| "未找到下载的更新包。")?;
    let signature = fs::read(&sig).map_err(|_| "未找到更新签名。")?;
    verify(tag, &binary, &signature)?;
    let process = unsafe { OpenProcess(0x0010_0000, 0, pid) };
    if !process.is_null() {
        let status = unsafe { WaitForSingleObject(process, 15_000) };
        unsafe { CloseHandle(process) };
        if status != 0 {
            return Err("旧程序未退出，请关闭后重试。");
        }
    }
    if backup.exists() {
        return Err("旧版备份仍然存在，请先检查上一次更新。");
    }
    let _ = fs::remove_file(&ready);
    fs::rename(path, &backup).map_err(|_| "无法备份旧版本。")?;
    if fs::rename(&staged, path).is_err() {
        fs::rename(&backup, path).map_err(|_| "无法恢复旧版，备份文件后缀为 .old。")?;
        let _ = Command::new(path).spawn();
        return Err("无法替换程序，已恢复旧版。");
    }
    let child = Command::new(path).arg("--update-ready").spawn();
    if !child.is_ok_and(|child| wait_for_ready(child, &ready)) {
        fs::remove_file(path)
            .and_then(|_| fs::rename(&backup, path))
            .map_err(|_| "无法恢复旧版，备份文件后缀为 .old。")?;
        let _ = Command::new(path).spawn();
        return Err("新版启动失败，已恢复旧版。");
    }
    for file in [backup, sig, ready] {
        let _ = fs::remove_file(file);
    }
    Ok(())
}

fn wait_for_ready(mut child: Child, ready: &Path) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if ready.is_file() {
            return true;
        }
        if child.try_wait().is_ok_and(|status| status.is_some()) {
            return false;
        }
        thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    let _ = child.wait();
    false
}

pub fn ready() {
    if let Ok(path) = std::env::current_exe() {
        let _ = write_new(&sibling(&path, ".ready"), b"ready");
    }
}

pub fn clean_helper() {
    if let Ok(path) = std::env::current_exe()
        && !sibling(&path, ".old").exists()
    {
        let _ = fs::remove_file(sibling(&path, ".update.exe"));
    }
}
