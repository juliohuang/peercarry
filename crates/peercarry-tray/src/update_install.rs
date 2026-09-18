//! Transactional self-update maintenance helper.
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::{
    fs,
    io::{Read, Write},
    net::{TcpStream, ToSocketAddrs},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use sysinfo::{Pid, ProcessesToUpdate, System};
const FLAG: &str = "--apply-staged-update";
const TOKEN_ENV: &str = "PEERCARRY_UPDATE_HEALTH_TOKEN";
const RESTART_ENV: &str = "PEERCARRY_RESTART";

pub fn run_helper_if_requested() -> Result<bool> {
    let mut a = std::env::args_os();
    let _ = a.next();
    if a.next().as_deref() != Some(std::ffi::OsStr::new(FLAG)) {
        return Ok(false);
    }
    let p = a.next().ok_or_else(|| anyhow!("missing update plan"))?;
    if a.next().is_some() {
        bail!("unexpected update helper arguments")
    }
    let path = Path::new(&p);
    if let Err(error) = apply_plan(path) {
        if let Some(dir) = path.parent() {
            if !dir.join("status.json").exists() {
                write_status(dir, "failed");
            }
        }
        return Err(error);
    }
    Ok(true)
}

pub fn launch(staged: &Path, version: &str, port: u16, token: Option<&str>) -> Result<()> {
    #[cfg(target_os = "macos")]
    if std::env::current_exe()?
        .components()
        .any(|c| c.as_os_str().to_string_lossy().ends_with(".app"))
    {
        bail!("macOS app-bundle self-update is unsupported")
    }
    let target = fs::canonicalize(std::env::current_exe()?)?;
    let parent = target
        .parent()
        .ok_or_else(|| anyhow!("target has no parent"))?;
    let staged = fs::canonicalize(staged)?;
    if !fs::metadata(&staged)?.is_file() {
        bail!("staged update is not a regular file")
    }
    let dir = unique_dir(parent)?;
    fs::create_dir(&dir)?;
    let copied = dir.join("newbinary");
    fs::copy(&staged, &copied)?;
    if sha256_file(&staged)? != sha256_file(&copied)? {
        bail!("staging copy verification failed");
    }
    let helper = dir.join(if cfg!(windows) {
        "helper.exe"
    } else {
        "helper"
    });
    fs::copy(&target, &helper)?;
    let plan = json!({"oldpid":std::process::id(),"target":target,"staged":copied,"expectedsha256":sha256_file(&copied)?,"newversion":version,"port":port});
    let raw = serde_json::to_vec(&plan)?;
    if raw.len() > 65536 {
        bail!("update plan too large")
    }
    let pp = dir.join("plan.json");
    fs::write(&pp, raw)?;
    let mut c = Command::new(&helper);
    c.args([FLAG, pp.to_string_lossy().as_ref()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(t) = token {
        if t.contains(['\r', '\n']) {
            bail!("invalid health token")
        }
        c.env(TOKEN_ENV, t);
    }
    hide(&mut c);
    let mut h = c.spawn()?;
    let ready = dir.join("ready");
    let end = Instant::now() + Duration::from_secs(2);
    while !ready.exists() && Instant::now() < end {
        if h.try_wait()?.is_some() {
            bail!("helper exited before ready")
        }
        thread::sleep(Duration::from_millis(25))
    }
    if !ready.exists() {
        let _ = h.kill();
        let _ = h.wait();
        bail!("helper ready timeout")
    }
    // The helper has its own verified copy; the download is no longer needed.
    let _ = fs::remove_file(staged);
    Ok(())
}

fn apply_plan(pp: &Path) -> Result<()> {
    if fs::symlink_metadata(pp)?.len() > 65536 {
        bail!("update plan too large")
    }
    let mut raw = Vec::new();
    fs::File::open(pp)?.take(65537).read_to_end(&mut raw)?;
    if raw.len() > 65536 {
        bail!("update plan too large");
    }
    let p: Value = serde_json::from_slice(&raw)?;
    let dir = pp.parent().ok_or_else(|| anyhow!("plan has no parent"))?;
    let target = pathv(&p, "target")?;
    let staged = pathv(&p, "staged")?;
    if target.parent() != dir.parent()
        || target.file_name().is_none()
        || !target.is_absolute()
        || !staged.is_absolute()
        || staged.parent() != Some(dir)
    {
        bail!("invalid update target")
    }
    let tm = fs::symlink_metadata(&target)?;
    if !tm.is_file() || tm.file_type().is_symlink() {
        bail!("target is not regular")
    }
    let sm = fs::symlink_metadata(&staged)?;
    if !sm.is_file() || sm.file_type().is_symlink() || sm.len() > 256 * 1024 * 1024 {
        bail!("invalid staged executable");
    }
    let pid = u32v(&p, "oldpid")?;
    if pid == 0 || pid == std::process::id() {
        bail!("invalid parent process");
    }
    let port = u16v(&p, "port")?;
    if port == 0 {
        bail!("invalid health port");
    }
    let version = strv(&p, "newversion")?;
    if sha256_file(&staged)? != strv(&p, "expectedsha256")? {
        bail!("staged hash changed");
    }
    fs::write(dir.join("ready"), b"ready")?;
    wait_exit(pid)?;
    let backup = target.with_extension(format!("old-{}", std::process::id()));
    let prepare = (|| -> Result<()> {
        if sha256_file(&staged)? != strv(&p, "expectedsha256")? {
            bail!("staged hash changed")
        }
        swap_in(&target, &backup)
    })();
    if let Err(error) = prepare {
        start_old(&target).context("update preparation failed; restarting old version")?;
        write_status(dir, "unchanged_restarted");
        return Err(error);
    }
    if let Err(e) = install_and_verify(&staged, &target, version, port) {
        if let Err(r) = rollback(&target, &backup) {
            write_status(dir, "rollback_error");
            return Err(anyhow!("update failed: {e}; rollback failed: {r}"));
        }
        if let Err(restart_error) = start_old(&target) {
            write_status(dir, "restored_but_restart_failed");
            return Err(restart_error.context("old executable restored but failed to start"));
        }
        write_status(dir, "rolled_back");
        return Err(e);
    }
    let _ = fs::remove_file(&staged);
    write_status(dir, "ok");
    let _ = fs::remove_file(pp);
    Ok(())
}
fn swap_in(target: &Path, backup: &Path) -> Result<()> {
    fs::rename(target, backup).context("backup target")
}
fn rollback(target: &Path, backup: &Path) -> Result<()> {
    if target.exists() {
        fs::remove_file(target).context("remove failed update")?;
    }
    fs::rename(backup, target).context("restore backup")
}
fn install_and_verify(s: &Path, t: &Path, v: &str, port: u16) -> Result<()> {
    fs::copy(s, t)?;
    let mut c = spawn_new(t)?;
    let end = Instant::now() + Duration::from_secs(15);
    loop {
        if c.try_wait()?.is_some() {
            bail!("new process exited")
        }
        if Instant::now() >= end {
            let _ = c.kill();
            let _ = c.wait();
            bail!("health timeout")
        }
        if hello(port).as_deref() == Some(v) {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(100))
    }
}
fn spawn_new(t: &Path) -> Result<Child> {
    let mut c = Command::new(t);
    c.env_remove(TOKEN_ENV).env_remove(RESTART_ENV);
    c.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    hide(&mut c);
    Ok(c.spawn()?)
}
fn start_old(t: &Path) -> Result<()> {
    let _ = spawn_new(t)?;
    Ok(())
}
fn hello(port: u16) -> Option<String> {
    let a = ("127.0.0.1", port).to_socket_addrs().ok()?.next()?;
    let mut s = TcpStream::connect_timeout(&a, Duration::from_millis(300)).ok()?;
    s.set_read_timeout(Some(Duration::from_millis(400))).ok()?;
    let t = std::env::var(TOKEN_ENV).ok();
    if t.as_deref().is_some_and(|v| v.contains(['\r', '\n'])) {
        return None;
    }
    let mut q = String::from("GET /v1/hello HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n");
    if let Some(t) = t {
        q.push_str(&format!("x-syncclip-token: {t}\r\n"));
    }
    q.push_str("\r\n");
    s.write_all(q.as_bytes()).ok()?;
    let mut b = Vec::new();
    s.take(65537).read_to_end(&mut b).ok()?;
    if b.len() > 65536 {
        return None;
    }
    let x = std::str::from_utf8(&b).ok()?;
    if !(x.starts_with("HTTP/1.1 200 ") || x.starts_with("HTTP/1.0 200 ")) {
        return None;
    }
    serde_json::from_str::<Value>(x.split("\r\n\r\n").nth(1)?)
        .ok()?
        .get("version")?
        .as_str()
        .map(str::to_owned)
}
fn wait_exit(pid: u32) -> Result<()> {
    let end = Instant::now() + Duration::from_secs(60);
    while alive(pid)? {
        if Instant::now() >= end {
            bail!("old process did not exit")
        }
        thread::sleep(Duration::from_millis(100))
    }
    Ok(())
}
fn alive(pid: u32) -> Result<bool> {
    let mut s = System::new();
    let p = Pid::from_u32(pid);
    s.refresh_processes(ProcessesToUpdate::Some(&[p]), true);
    Ok(s.process(p).is_some())
}
fn sha256_file(p: &Path) -> Result<String> {
    let mut f = fs::File::open(p)?;
    let mut h = Sha256::new();
    let mut b = [0u8; 65536];
    loop {
        let n = f.read(&mut b)?;
        if n == 0 {
            break;
        }
        h.update(&b[..n])
    }
    Ok(hex::encode(h.finalize()))
}
fn unique_dir(p: &Path) -> Result<PathBuf> {
    for i in 0..32 {
        let d = p.join(format!(".peercarry-update-{}-{i}", std::process::id()));
        if !d.exists() {
            return Ok(d);
        }
    }
    bail!("cannot allocate update directory")
}
fn write_status(d: &Path, s: &str) {
    let _ = fs::write(d.join("status.json"), json!({"status":s}).to_string());
}
fn strv<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v.get(k)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("invalid plan field {k}"))
}
fn pathv(v: &Value, k: &str) -> Result<PathBuf> {
    Ok(PathBuf::from(strv(v, k)?))
}
fn u32v(v: &Value, k: &str) -> Result<u32> {
    v.get(k)
        .and_then(Value::as_u64)
        .and_then(|x| u32::try_from(x).ok())
        .ok_or_else(|| anyhow!("invalid plan field {k}"))
}
fn u16v(v: &Value, k: &str) -> Result<u16> {
    v.get(k)
        .and_then(Value::as_u64)
        .and_then(|x| u16::try_from(x).ok())
        .ok_or_else(|| anyhow!("invalid plan field {k}"))
}
#[cfg(windows)]
fn hide(c: &mut Command) {
    c.creation_flags(0x08000000);
}
#[cfg(not(windows))]
fn hide(_: &mut Command) {}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hash_fixture() {
        let d = std::env::temp_dir().join(format!("peercarry-{}", std::process::id()));
        let _ = fs::create_dir(&d);
        let p = d.join("x");
        fs::write(&p, b"abc").unwrap();
        assert_eq!(
            sha256_file(&p).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let _ = fs::remove_dir_all(d);
    }
    #[test]
    fn production_swap_fixture() {
        let d = std::env::temp_dir().join(format!("peercarry-swap-{}", std::process::id()));
        let _ = fs::create_dir(&d);
        let t = d.join("app");
        let s = d.join("stage");
        let b = d.join("old");
        fs::write(&t, b"old").unwrap();
        fs::write(&s, b"new").unwrap();
        swap_in(&t, &b).unwrap();
        fs::copy(&s, &t).unwrap();
        assert_eq!(fs::read(&t).unwrap(), b"new");
        rollback(&t, &b).unwrap();
        assert_eq!(fs::read(&t).unwrap(), b"old");
        let _ = fs::remove_dir_all(d);
    }
}
