//! Starting a tool's watcher detached from the terminal or editor that asked for it, and
//! finding and stopping it again by its pid file.

use std::fs::OpenOptions;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

use crate::state::Daemon;

const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

pub fn running(daemon: &Daemon) -> Option<u32> {
    let pid: u32 = std::fs::read_to_string(&daemon.pid)
        .ok()?
        .trim()
        .parse()
        .ok()?;
    is_alive(pid).then_some(pid)
}

/// `command`'s output goes to the daemon's log. `what` names it in the error when one runs already.
pub fn start(daemon: &Daemon, mut command: Command, what: &str) -> Result<u32> {
    if let Some(pid) = running(daemon) {
        bail!("{what} is already running (pid {pid})");
    }
    prepare_log(&daemon.log)?;
    let output = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&daemon.log)
        .with_context(|| format!("cannot open {}", daemon.log.display()))?;
    let errors = output
        .try_clone()
        .context("cannot duplicate the log handle")?;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::from(output))
        .stderr(Stdio::from(errors));
    detach(&mut command);
    keep_std_handles_from_the_child();

    let pid = command
        .spawn()
        .with_context(|| format!("cannot start {what}"))?
        .id();
    if let Some(parent) = daemon.pid.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    std::fs::write(&daemon.pid, pid.to_string())
        .with_context(|| format!("cannot write {}", daemon.pid.display()))?;
    let _ = std::fs::remove_file(&daemon.heartbeat);
    Ok(pid)
}

/// `None` when none was running.
pub fn stop(daemon: &Daemon) -> Result<Option<u32>> {
    let Some(pid) = running(daemon) else {
        let _ = std::fs::remove_file(&daemon.pid);
        return Ok(None);
    };
    terminate(pid)?;
    let _ = std::fs::remove_file(&daemon.pid);
    let _ = std::fs::remove_file(&daemon.heartbeat);
    Ok(Some(pid))
}

/// Every live one under `dir`, by the identity its pid file is named after. Dead ones are cleared.
pub fn running_in(dir: &Path) -> Vec<(String, u32)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("pid") {
            continue;
        }
        let Some(identity) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        let pid = std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| raw.trim().parse::<u32>().ok());
        match pid {
            Some(pid) if is_alive(pid) => found.push((identity.to_string(), pid)),
            _ => {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    found.sort();
    found
}

/// `running (pid 42), heartbeat 3s ago`, or `stopped`.
pub fn describe(daemon: &Daemon) -> String {
    match running(daemon) {
        None => "stopped".to_string(),
        Some(pid) => match daemon.heartbeat_age() {
            Some(age) if !daemon.is_beating() => {
                format!("running (pid {pid}), last heartbeat {age}s ago")
            }
            Some(age) => format!("running (pid {pid}), heartbeat {age}s ago"),
            None => format!("running (pid {pid}), starting up"),
        },
    }
}

fn prepare_log(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    let size = std::fs::metadata(path)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    if size > MAX_LOG_BYTES {
        std::fs::write(path, "").with_context(|| format!("cannot truncate {}", path.display()))?;
    }
    Ok(())
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

/// An editor's pipes would otherwise stay open in the child, and the editor would wait on it.
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
pub fn is_alive(pid: u32) -> bool {
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
pub fn is_alive(pid: u32) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> (std::path::PathBuf, Daemon) {
        let dir =
            std::env::temp_dir().join(format!("sfcc-core-daemon-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let daemon = Daemon::named(&dir.join("daemons"), &dir.join("logs"), "sbx__v1");
        (dir, daemon)
    }

    #[test]
    fn a_pid_file_of_a_process_that_is_gone_is_not_running_and_is_cleared() {
        let (dir, daemon) = scratch("gone");
        std::fs::create_dir_all(daemon.pid.parent().unwrap()).unwrap();
        std::fs::write(&daemon.pid, u32::MAX.to_string()).unwrap();
        assert_eq!(running(&daemon), None);
        assert_eq!(describe(&daemon), "stopped");
        assert!(running_in(daemon.pid.parent().unwrap()).is_empty());
        assert!(!daemon.pid.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn this_process_is_found_by_its_pid_file() {
        let (dir, daemon) = scratch("alive");
        std::fs::create_dir_all(daemon.pid.parent().unwrap()).unwrap();
        std::fs::write(&daemon.pid, std::process::id().to_string()).unwrap();
        assert_eq!(running(&daemon), Some(std::process::id()));
        assert_eq!(
            running_in(daemon.pid.parent().unwrap()),
            vec![("sbx__v1".to_string(), std::process::id())]
        );
        daemon.beat();
        assert!(describe(&daemon).contains("heartbeat 0s ago"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
