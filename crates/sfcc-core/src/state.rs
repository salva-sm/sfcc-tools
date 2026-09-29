//! Where each tool keeps its state, and the files one tool writes for another to read.

use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// sfcc-upload's manifests, logs, daemons and watcher status. Moves `prost`'s over once.
pub fn uploader_dir() -> PathBuf {
    let root = state_root();
    let current = root.join("sfcc-upload");
    if !current.exists() {
        let old = root.join("prost");
        if old.is_dir() {
            let _ = std::fs::rename(&old, &current);
        }
    }
    current
}

pub fn log_diff_dir() -> PathBuf {
    config_root().join("log-diff")
}

pub fn debugger_dir() -> PathBuf {
    config_root().join("sfcc-dap")
}

fn state_root() -> PathBuf {
    if cfg!(windows)
        && let Ok(local) = std::env::var("LOCALAPPDATA")
    {
        return PathBuf::from(local);
    }
    match std::env::var("XDG_STATE_HOME") {
        Ok(state) if !state.is_empty() => PathBuf::from(state),
        _ => home().join(".local").join("state"),
    }
}

fn config_root() -> PathBuf {
    if cfg!(windows)
        && let Ok(appdata) = std::env::var("APPDATA")
    {
        return PathBuf::from(appdata);
    }
    match std::env::var("XDG_CONFIG_HOME") {
        Ok(config) if !config.is_empty() => PathBuf::from(config),
        _ => home().join(".config"),
    }
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".to_string()))
}

/// The watcher's state, one file per sandbox and code version.
pub mod upload {
    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "lowercase")]
    pub enum State {
        Synced,
        Uploading,
        /// The changes are still queued, or were held.
        Failed,
        Stopped,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct Status {
        pub state: State,
        /// Absolute: how a workspace recognises its own watcher.
        pub cartridges: String,
        pub hostname: String,
        #[serde(default)]
        pub code_version: String,
        /// Files in the batch the state refers to.
        #[serde(default)]
        pub files: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub detail: Option<String>,
        pub at: i64,
    }

    /// A watcher beats every 20 s; past this with no word it is gone, not quiet.
    pub const STALE_SECONDS: i64 = 90;

    pub fn dir() -> PathBuf {
        uploader_dir().join("status")
    }

    pub fn path(identity: &str) -> PathBuf {
        dir().join(format!("{identity}.json"))
    }

    pub fn all() -> Vec<Status> {
        read_all(&dir())
    }

    pub fn daemons_dir() -> PathBuf {
        uploader_dir().join("daemons")
    }

    pub fn pid_path(identity: &str) -> PathBuf {
        daemons_dir().join(format!("{identity}.pid"))
    }

    pub fn heartbeat_path(identity: &str) -> PathBuf {
        daemons_dir().join(format!("{identity}.beat"))
    }

    pub fn log_path(identity: &str) -> PathBuf {
        uploader_dir().join("logs").join(format!("{identity}.log"))
    }

    pub fn manifest_path(identity: &str) -> PathBuf {
        uploader_dir()
            .join("manifests")
            .join(format!("{identity}.json"))
    }

    pub fn mark_path(identity: &str) -> PathBuf {
        uploader_dir()
            .join("marks")
            .join(format!("{identity}.json"))
    }

    pub fn write_heartbeat(path: &Path) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, now_seconds().to_string());
    }

    pub fn heartbeat_age(path: &Path) -> Option<i64> {
        let stamp: i64 = std::fs::read_to_string(path).ok()?.trim().parse().ok()?;
        Some(now_seconds() - stamp)
    }
}

/// What log-diff last found pending on a sandbox.
pub mod errors {
    use super::*;

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct Status {
        pub cartridges: String,
        pub hostname: String,
        pub pending: usize,
        /// Of `pending`, what the last pass turned up.
        #[serde(default)]
        pub new: usize,
        pub at: i64,
    }

    /// A check a day old says nothing about now; `watch` writes every few seconds.
    pub const STALE_SECONDS: i64 = 24 * 60 * 60;

    pub fn dir() -> PathBuf {
        log_diff_dir().join("status")
    }

    pub fn path(identity: &str) -> PathBuf {
        dir().join(format!("{identity}.json"))
    }

    pub fn all() -> Vec<Status> {
        read_all(&dir())
    }
}

/// A debug session isml-lsp can ask for values on localhost.
pub mod sessions {
    use super::*;

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct Session {
        pub cartridges: String,
        pub port: u16,
        pub token: String,
    }

    pub fn dir() -> PathBuf {
        debugger_dir().join("sessions")
    }

    pub fn path(pid: u32) -> PathBuf {
        dir().join(format!("{pid}.json"))
    }

    pub fn all() -> Vec<Session> {
        read_all(&dir())
    }
}

/// Never fails what it reports on: an unwritable status is no reason to stop.
pub fn write<T: Serialize>(path: &Path, value: &T) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(body) = serde_json::to_string(value) {
        let _ = std::fs::write(path, body);
    }
}

pub fn read<T: DeserializeOwned>(path: &Path) -> Option<T> {
    if path.extension().is_none_or(|extension| extension != "json") {
        return None;
    }
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn read_all<T: DeserializeOwned>(dir: &Path) -> Vec<T> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| read(&entry.path()))
        .collect()
}

/// Separators, and on Windows case, aside: `dw.json` and an editor rarely spell a path alike.
pub fn is_within(inner: &Path, outer: &Path) -> bool {
    let normal = |path: &Path| {
        let text = path.to_string_lossy().replace('\\', "/");
        let text = text.trim_end_matches('/').to_string();
        match cfg!(windows) {
            true => text.to_lowercase(),
            false => text,
        }
    };
    let (inner, outer) = (normal(inner), normal(outer));
    !outer.is_empty() && (inner == outer || inner.starts_with(&format!("{outer}/")))
}

pub fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_status_round_trips_through_the_json_another_tool_reads() {
        let status = upload::Status {
            state: upload::State::Uploading,
            cartridges: "C:/repo/cartridges".into(),
            hostname: "sbx-001.dx.commercecloud.salesforce.com".into(),
            code_version: "version1".into(),
            files: 7,
            detail: None,
            at: 1_700_000_000,
        };
        let body = serde_json::to_string(&status).unwrap();
        assert!(body.contains(r#""state":"uploading""#));
        assert!(!body.contains("detail"));
        assert_eq!(
            serde_json::from_str::<upload::Status>(&body).unwrap(),
            status
        );
    }

    #[test]
    fn reads_every_json_file_in_a_folder_and_skips_the_rest() {
        let dir = std::env::temp_dir().join(format!("sfcc-core-state-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let session = sessions::Session {
            cartridges: "/repo/cartridges".into(),
            port: 4000,
            token: "t".into(),
        };
        write(&dir.join("1.json"), &session);
        std::fs::write(dir.join("2.json"), "not json").unwrap();
        std::fs::write(dir.join("3.txt"), "{}").unwrap();
        assert_eq!(read_all::<sessions::Session>(&dir), vec![session]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_path_is_within_a_folder_however_it_is_spelled() {
        let file = Path::new(r"C:\Dev\shop\cartridges\app\cartridge\a.js");
        assert!(is_within(file, Path::new(r"C:\Dev\shop\cartridges\")));
        assert!(is_within(file, Path::new("C:/Dev/shop/cartridges")));
        assert!(!is_within(file, Path::new(r"C:\Dev\shop\cartridges-old")));
        assert!(!is_within(file, Path::new("")));
        if cfg!(windows) {
            assert!(is_within(file, Path::new("c:/dev/shop/cartridges")));
        }
    }
}
