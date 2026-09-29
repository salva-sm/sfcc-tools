use anyhow::{Context, Result};
use sfcc_core::config::Config;
use sfcc_core::daemon;
use sfcc_core::state::{Daemon, upload};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::process::Command;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct SpawnArgs {
    pub watch: crate::watch::WatchOptions,
    pub jobs: usize,
    pub cartridges: Vec<String>,
}

pub fn of(config: &Config) -> Daemon {
    upload::daemon(&config.identity())
}

pub fn running_pid(config: &Config) -> Option<u32> {
    daemon::running(&of(config))
}

pub fn start(config: &Config, args: SpawnArgs) -> Result<u32> {
    let executable = std::env::current_exe().context("cannot locate the sfcc-upload executable")?;
    let mut command = Command::new(executable);
    command
        .arg("--config")
        .arg(&config.dw_json)
        .arg("--code-version")
        .arg(&config.code_version)
        .arg("watch")
        .arg("--jobs")
        .arg(args.jobs.to_string());
    if args.watch.full {
        command.arg("--full");
    }
    if !args.watch.initial_push {
        command.arg("--no-initial-push");
    }
    if let Some(port) = args.watch.reload_port {
        command
            .arg("--reload")
            .arg("--reload-port")
            .arg(port.to_string());
    }
    for cartridge in &args.cartridges {
        command.arg("--cartridge").arg(cartridge);
    }
    let what = format!("a watcher for {}", config.code_version);
    daemon::start(&of(config), command, &what)
}

pub fn stop(config: &Config) -> Result<Option<u32>> {
    let stopped = daemon::stop(&of(config))?;
    if stopped.is_some() {
        // Killed, it cannot clear its own status: an editor would show it uploading until it went stale.
        crate::sync_status::clear(config);
    }
    Ok(stopped)
}

/// Found by its pid file alone, so it can be stopped without its dw.json.
#[derive(Debug, Clone)]
pub struct Running {
    /// The sandbox and code version, as the state files are named.
    pub identity: String,
    pub pid: u32,
    pub description: Option<String>,
}

pub fn running_anywhere() -> Vec<Running> {
    daemon::running_in(&upload::daemons_dir())
        .into_iter()
        .map(|(identity, pid)| Running {
            description: describe_identity(&identity),
            identity,
            pid,
        })
        .collect()
}

/// `hostname / code version - cartridges folder`, from the watcher's status file.
fn describe_identity(identity: &str) -> Option<String> {
    let status: upload::Status = sfcc_core::state::read(&upload::path(identity))?;
    Some(format!(
        "{} / {} - {}",
        status.hostname, status.code_version, status.cartridges
    ))
}

pub fn stop_running(watcher: &Running) -> Result<()> {
    daemon::stop(&upload::daemon(&watcher.identity))?;
    let _ = std::fs::remove_file(upload::path(&watcher.identity));
    Ok(())
}

pub fn tail(config: &Config, lines: usize) -> Result<String> {
    let path = of(config).log;
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return Ok(String::new());
    };
    let collected: Vec<&str> = contents.lines().collect();
    let start = collected.len().saturating_sub(lines);
    Ok(collected[start..].join("\n"))
}

pub fn follow(config: &Config, lines: usize) -> Result<()> {
    let path = of(config).log;
    crate::out!("{}", tail(config, lines)?);

    let mut file = File::open(&path).with_context(|| format!("cannot open {}", path.display()))?;
    let mut position = file.seek(SeekFrom::End(0)).context("cannot seek the log")?;
    let mut buffer = String::new();

    loop {
        std::thread::sleep(Duration::from_millis(400));
        let length = std::fs::metadata(&path)
            .map(|metadata| metadata.len())
            .unwrap_or(position);
        if length < position {
            position = 0;
        }
        if length == position {
            continue;
        }
        file.seek(SeekFrom::Start(position))
            .context("cannot seek the log")?;
        buffer.clear();
        file.read_to_string(&mut buffer)
            .context("cannot read the log")?;
        crate::outp!("{buffer}");
        std::io::stdout().flush().ok();
        position = length;
    }
}

pub fn describe_state(config: &Config) -> String {
    daemon::describe(&of(config))
}
