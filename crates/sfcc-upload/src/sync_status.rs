//! What the watcher is doing, written where an editor can read it.
//!
//! Nothing in an editor can see a detached watcher, so a save that failed to
//! upload looks exactly like one that worked. The watcher writes a one-line
//! status next to its pid file after every transition; a language server
//! polling that file can put the state in the status bar.
//!
//! The file is keyed by the cartridges directory rather than by the sandbox,
//! so a reader that knows only the folder it has open can find the right one
//! without reproducing how the sandbox identity is derived.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::manifest::state_dir;
use sfcc_core::config::Config;

/// Where the status files live, one per watcher.
pub fn status_dir() -> PathBuf {
    state_dir().join("status")
}

pub fn status_path(config: &Config) -> PathBuf {
    status_dir().join(format!("{}.json", config.identity()))
}

/// What the watcher is doing right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// Everything on disk is on the sandbox.
    Synced,
    /// An upload is in flight.
    Uploading,
    /// The last upload failed, and the changes are still queued.
    Failed,
    /// No watcher is running for this folder.
    Stopped,
}

/// One watcher's state, as an editor reads it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Status {
    pub state: State,
    /// Absolute path of the cartridges directory being watched.
    pub cartridges: String,
    /// The sandbox, for a reader that wants to name it.
    pub hostname: String,
    pub code_version: String,
    /// Files in the batch the state refers to.
    #[serde(default)]
    pub files: usize,
    /// Why it failed, when it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Seconds since the epoch, so a reader can tell a stale file from a live one.
    pub at: i64,
}

impl Status {
    fn new(config: &Config, state: State) -> Status {
        Status {
            state,
            cartridges: config.cartridges_dir.to_string_lossy().into_owned(),
            hostname: config.hostname.clone(),
            code_version: config.code_version.clone(),
            files: 0,
            detail: None,
            at: now_seconds(),
        }
    }
}

/// Record a state. Never fails the upload it is reporting on: a status nobody
/// can write is a status nobody reads, not a reason to stop.
pub fn publish(config: &Config, state: State) {
    write(&Status::new(config, state), &status_path(config));
}

pub fn publish_uploading(config: &Config, files: usize) {
    let mut status = Status::new(config, State::Uploading);
    status.files = files;
    write(&status, &status_path(config));
}

pub fn publish_failure(config: &Config, detail: String) {
    let mut status = Status::new(config, State::Failed);
    status.detail = Some(detail);
    write(&status, &status_path(config));
}

/// Leave the folder as a reader finds it when no watcher runs.
pub fn clear(config: &Config) {
    let _ = std::fs::remove_file(status_path(config));
}

fn write(status: &Status, path: &Path) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(body) = serde_json::to_string(status) {
        let _ = std::fs::write(path, body);
    }
}

fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_the_json_an_editor_reads() {
        let status = Status {
            state: State::Uploading,
            cartridges: "C:/repo/cartridges".into(),
            hostname: "sbx-001.dx.commercecloud.salesforce.com".into(),
            code_version: "version1".into(),
            files: 7,
            detail: None,
            at: 1_700_000_000,
        };
        let body = serde_json::to_string(&status).unwrap();
        assert!(body.contains(r#""state":"uploading""#));

        let read: Status = serde_json::from_str(&body).unwrap();
        assert_eq!(read.state, State::Uploading);
        assert_eq!(read.files, 7);
        assert_eq!(read.cartridges, "C:/repo/cartridges");
    }

    #[test]
    fn keeps_a_failure_reason_and_drops_an_absent_one() {
        let mut status = Status {
            state: State::Failed,
            cartridges: "/repo/cartridges".into(),
            hostname: "host".into(),
            code_version: "version1".into(),
            files: 0,
            detail: Some("502 Bad Gateway".into()),
            at: 0,
        };
        assert!(
            serde_json::to_string(&status)
                .unwrap()
                .contains("502 Bad Gateway")
        );

        status.detail = None;
        assert!(!serde_json::to_string(&status).unwrap().contains("detail"));
    }
}
