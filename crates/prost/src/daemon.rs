use crate::config::Config;
use crate::manifest::state_dir;
use anyhow::{Context, Result, bail};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;
const STALE_HEARTBEAT: i64 = 90;

#[derive(Debug, Clone)]
pub struct SpawnArgs {
    pub watch: crate::watch::WatchOptions,
    pub jobs: usize,
    pub cartridges: Vec<String>,
}

pub fn pid_path(config: &Config) -> PathBuf {
    state_dir().join("daemons").join(format!("{}.pid", config.identity()))
}

pub fn log_path(config: &Config) -> PathBuf {
    state_dir().join("logs").join(format!("{}.log", config.identity()))
}

pub fn heartbeat_path(config: &Config) -> PathBuf {
    state_dir().join("daemons").join(format!("{}.beat", config.identity()))
}

pub fn write_heartbeat(path: &Path) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, now_seconds().to_string());
}

pub fn heartbeat_age(path: &Path) -> Option<i64> {
    let raw = std::fs::read_to_string(path).ok()?;
    let stamp: i64 = raw.trim().parse().ok()?;
    Some(now_seconds() - stamp)
}

pub fn running_pid(config: &Config) -> Option<u32> {
    let raw = std::fs::read_to_string(pid_path(config)).ok()?;
    let pid: u32 = raw.trim().parse().ok()?;
    if is_alive(pid) { Some(pid) } else { None }
}

pub fn start(config: &Config, args: SpawnArgs) -> Result<u32> {
    if let Some(pid) = running_pid(config) {
        bail!("a watcher is already running for {} (pid {pid})", config.code_version);
    }

    let executable = std::env::current_exe().context("cannot locate the prost executable")?;
    let log = log_path(config);
    prepare_log(&log)?;

    let output = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)
        .with_context(|| format!("cannot open {}", log.display()))?;
    let errors = output.try_clone().context("cannot duplicate the log handle")?;

    let mut command = Command::new(executable);
    command
        .arg("--config")
        .arg(&config.dw_json)
        .arg("--code-version")
        .arg(&config.code_version)
        .arg("watch")
        .arg("--jobs")
        .arg(args.jobs.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::from(output))
        .stderr(Stdio::from(errors));

    if args.watch.full {
        command.arg("--full");
    }
    if !args.watch.initial_push {
        command.arg("--no-initial-push");
    }
    if let Some(port) = args.watch.reload_port {
        command.arg("--reload").arg("--reload-port").arg(port.to_string());
    }
    for cartridge in &args.cartridges {
        command.arg("--cartridge").arg(cartridge);
    }
    detach(&mut command);
    keep_std_handles_from_the_child();

    let child = command.spawn().context("cannot start the background watcher")?;
    let pid = child.id();

    let pid_file = pid_path(config);
    if let Some(parent) = pid_file.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("cannot create {}", parent.display()))?;
    }
    std::fs::write(&pid_file, pid.to_string())
        .with_context(|| format!("cannot write {}", pid_file.display()))?;
    let _ = std::fs::remove_file(heartbeat_path(config));

    Ok(pid)
}

pub fn stop(config: &Config) -> Result<Option<u32>> {
    let pid_file = pid_path(config);
    let Some(pid) = running_pid(config) else {
        let _ = std::fs::remove_file(&pid_file);
        return Ok(None);
    };

    terminate(pid)?;
    let _ = std::fs::remove_file(&pid_file);
    let _ = std::fs::remove_file(heartbeat_path(config));
    Ok(Some(pid))
}

pub fn tail(config: &Config, lines: usize) -> Result<String> {
    let path = log_path(config);
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return Ok(String::new());
    };
    let collected: Vec<&str> = contents.lines().collect();
    let start = collected.len().saturating_sub(lines);
    Ok(collected[start..].join("\n"))
}

pub fn follow(config: &Config, lines: usize) -> Result<()> {
    let path = log_path(config);
    crate::out!("{}", tail(config, lines)?);

    let mut file = File::open(&path).with_context(|| format!("cannot open {}", path.display()))?;
    let mut position = file.seek(SeekFrom::End(0)).context("cannot seek the log")?;
    let mut buffer = String::new();

    loop {
        std::thread::sleep(Duration::from_millis(400));
        let length = std::fs::metadata(&path).map(|metadata| metadata.len()).unwrap_or(position);
        if length < position {
            position = 0;
        }
        if length == position {
            continue;
        }
        file.seek(SeekFrom::Start(position)).context("cannot seek the log")?;
        buffer.clear();
        file.read_to_string(&mut buffer).context("cannot read the log")?;
        crate::outp!("{buffer}");
        std::io::stdout().flush().ok();
        position = length;
    }
}

pub fn describe_state(config: &Config) -> String {
    match running_pid(config) {
        None => "stopped".to_string(),
        Some(pid) => match heartbeat_age(&heartbeat_path(config)) {
            Some(age) if age > STALE_HEARTBEAT => format!("running (pid {pid}), last heartbeat {age}s ago"),
            Some(age) => format!("running (pid {pid}), heartbeat {age}s ago"),
            None => format!("running (pid {pid}), starting up"),
        },
    }
}

fn prepare_log(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("cannot create {}", parent.display()))?;
    }
    if std::fs::metadata(path).map(|metadata| metadata.len()).unwrap_or(0) > MAX_LOG_BYTES {
        std::fs::write(path, "").with_context(|| format!("cannot truncate {}", path.display()))?;
    }
    Ok(())
}

fn now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(windows)]
fn detach(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
}

#[cfg(unix)]
fn detach(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(windows)]
mod win32 {
    use std::ffi::c_void;

    pub type Handle = *mut c_void;
    pub const PROCESS_TERMINATE: u32 = 0x0001;
    pub const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    pub const STILL_ACTIVE: u32 = 259;
    pub const HANDLE_FLAG_INHERIT: u32 = 0x0001;
    pub const STD_HANDLES: [u32; 3] = [0xFFFF_FFF6, 0xFFFF_FFF5, 0xFFFF_FFF4];

    #[link(name = "kernel32")]
    unsafe extern "system" {
        pub fn OpenProcess(access: u32, inherit_handle: i32, process_id: u32) -> Handle;
        pub fn GetExitCodeProcess(process: Handle, exit_code: *mut u32) -> i32;
        pub fn TerminateProcess(process: Handle, exit_code: u32) -> i32;
        pub fn CloseHandle(object: Handle) -> i32;
        pub fn GetStdHandle(id: u32) -> Handle;
        pub fn SetHandleInformation(object: Handle, mask: u32, flags: u32) -> i32;
    }
}

#[cfg(windows)]
fn keep_std_handles_from_the_child() {
    for id in win32::STD_HANDLES {
        unsafe {
            let handle = win32::GetStdHandle(id);
            if !handle.is_null() && handle as isize != -1 {
                win32::SetHandleInformation(handle, win32::HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
}

#[cfg(unix)]
fn keep_std_handles_from_the_child() {}

#[cfg(windows)]
fn is_alive(pid: u32) -> bool {
    unsafe {
        let handle = win32::OpenProcess(win32::PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }
        let mut code: u32 = 0;
        let queried = win32::GetExitCodeProcess(handle, &mut code);
        win32::CloseHandle(handle);
        queried != 0 && code == win32::STILL_ACTIVE
    }
}

#[cfg(unix)]
fn is_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

#[cfg(windows)]
fn terminate(pid: u32) -> Result<()> {
    unsafe {
        let handle = win32::OpenProcess(win32::PROCESS_TERMINATE, 0, pid);
        if handle.is_null() {
            bail!("cannot open process {pid}");
        }
        let stopped = win32::TerminateProcess(handle, 0);
        win32::CloseHandle(handle);
        if stopped == 0 {
            bail!("cannot stop process {pid}");
        }
    }
    Ok(())
}

#[cfg(unix)]
fn terminate(pid: u32) -> Result<()> {
    if unsafe { libc::kill(pid as i32, libc::SIGTERM) } != 0 {
        bail!("cannot stop process {pid}");
    }
    Ok(())
}
