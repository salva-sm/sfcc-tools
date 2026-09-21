//! Whether the sandbox has the code that is on disk.
//!
//! A detached uploader is invisible from inside the editor, so a save that
//! failed to reach the sandbox looks exactly like one that worked — and the
//! next half hour goes into debugging code the instance never received.
//!
//! Zed has no status-bar API for an extension, but it does render LSP
//! progress, so that is the channel used here.
//!
//! # What it can and cannot show
//!
//! Progress is meant for work in flight, not for steady state. So this
//! reports the two states worth interrupting for — **uploading** and
//! **failed**, the latter staying up until an upload succeeds — and shows
//! nothing at all when everything is in sync. There is no always-on green
//! light, because a progress item that never ends reads as a stuck spinner.
//!
//! # The file it reads
//!
//! The uploader writes one JSON file per watcher under its own state
//! directory. This module knows that layout, which is the one thing the two
//! programs have to agree on until they share a binary.

use crossbeam_channel::{SendError, Sender};
use std::path::{Path, PathBuf};
use std::time::Duration;

use lsp_server::Message;
use lsp_types::notification::{Notification, Progress};
use lsp_types::request::{Request as RequestTrait, WorkDoneProgressCreate};
use lsp_types::{
    NumberOrString, ProgressParams, ProgressParamsValue, WorkDoneProgress, WorkDoneProgressBegin,
    WorkDoneProgressCreateParams, WorkDoneProgressEnd, WorkDoneProgressReport,
};

const TOKEN: &str = "sfcc/sync";
const POLL: Duration = Duration::from_millis(1500);
/// A watcher writes on every transition and beats every 20 s; past a minute
/// with no word, it is gone rather than quiet.
const STALE_SECONDS: i64 = 90;

/// What the uploader is doing, as its status file spells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
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

/// One watcher's state.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Status {
    /// What it is doing.
    pub state: State,
    /// The cartridges directory being watched, which is how a workspace
    /// recognises its own watcher.
    pub cartridges: String,
    /// The sandbox host.
    pub hostname: String,
    /// Files in the batch the state refers to.
    #[serde(default)]
    pub files: usize,
    /// Why it failed, when it did.
    #[serde(default)]
    pub detail: Option<String>,
    /// Seconds since the epoch.
    pub at: i64,
}

/// Follow the uploader in the background and report it to the editor, for as
/// long as the session lasts.
pub fn report(roots: Vec<PathBuf>, sender: Sender<Message>) {
    std::thread::spawn(move || {
        if create_token(&sender).is_err() {
            return;
        }
        let mut shown: Option<State> = None;
        loop {
            let state = current(&roots).map(|status| (status.state, describe(&status)));
            let (state, message) = match state {
                Some((state, message)) => (Some(state), message),
                None => (None, String::new()),
            };
            if state != shown && announce(&sender, shown, state, message).is_err() {
                return;
            }
            shown = state;
            std::thread::sleep(POLL);
        }
    });
}

/// The status of the watcher covering one of the open folders, if there is one.
pub fn current(roots: &[PathBuf]) -> Option<Status> {
    let directory = status_dir()?;
    let mut best: Option<Status> = None;
    for entry in std::fs::read_dir(directory).ok()?.flatten() {
        let Some(status) = read(&entry.path()) else {
            continue;
        };
        if !covers(&status, roots) || is_stale(&status) {
            continue;
        }
        if best.as_ref().is_none_or(|found| found.at < status.at) {
            best = Some(status);
        }
    }
    best
}

fn read(path: &Path) -> Option<Status> {
    if path.extension().is_none_or(|extension| extension != "json") {
        return None;
    }
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// A watcher belongs to this session when what it watches is inside a folder
/// the editor has open.
fn covers(status: &Status, roots: &[PathBuf]) -> bool {
    let watched = PathBuf::from(&status.cartridges);
    roots.iter().any(|root| watched.starts_with(root))
}

fn is_stale(status: &Status) -> bool {
    now_seconds() - status.at > STALE_SECONDS
}

/// The uploader's state directory, as it derives it.
fn status_dir() -> Option<PathBuf> {
    let root = match std::env::var("LOCALAPPDATA") {
        Ok(local) if cfg!(windows) => PathBuf::from(local),
        _ => PathBuf::from(std::env::var("XDG_STATE_HOME").ok()?),
    };
    Some(root.join("prost").join("status"))
}

fn describe(status: &Status) -> String {
    match status.state {
        State::Uploading => match status.files {
            0 => format!("uploading to {}", status.hostname),
            1 => format!("uploading 1 file to {}", status.hostname),
            files => format!("uploading {files} files to {}", status.hostname),
        },
        State::Failed => match &status.detail {
            Some(detail) => format!("upload failed — {detail}"),
            None => "upload failed".to_string(),
        },
        State::Synced | State::Stopped => String::new(),
    }
}

/// Only what is worth interrupting for reaches the status bar.
fn worth_showing(state: Option<State>) -> bool {
    matches!(state, Some(State::Uploading) | Some(State::Failed))
}

fn announce(
    sender: &Sender<Message>,
    was: Option<State>,
    now: Option<State>,
    message: String,
) -> Result<(), SendError<Message>> {
    let value = match (worth_showing(was), worth_showing(now)) {
        (false, true) => WorkDoneProgress::Begin(WorkDoneProgressBegin {
            title: "SFCC".to_string(),
            message: Some(message),
            ..Default::default()
        }),
        (true, true) => WorkDoneProgress::Report(WorkDoneProgressReport {
            message: Some(message),
            ..Default::default()
        }),
        (true, false) => WorkDoneProgress::End(WorkDoneProgressEnd { message: None }),
        (false, false) => return Ok(()),
    };
    sender.send(Message::Notification(lsp_server::Notification::new(
        Progress::METHOD.to_string(),
        ProgressParams {
            token: NumberOrString::String(TOKEN.to_string()),
            value: ProgressParamsValue::WorkDone(value),
        },
    )))
}

fn create_token(sender: &Sender<Message>) -> Result<(), SendError<Message>> {
    sender.send(Message::Request(lsp_server::Request::new(
        lsp_server::RequestId::from(TOKEN.to_string()),
        WorkDoneProgressCreate::METHOD.to_string(),
        WorkDoneProgressCreateParams {
            token: NumberOrString::String(TOKEN.to_string()),
        },
    )))
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

    fn status(state: State, at: i64) -> Status {
        Status {
            state,
            cartridges: "/repo/source/cartridges".into(),
            hostname: "sbx-001.example.com".into(),
            files: 7,
            detail: None,
            at,
        }
    }

    #[test]
    fn reads_what_the_uploader_writes() {
        let body = r#"{"state":"uploading","cartridges":"/repo/cartridges",
                       "hostname":"host","code_version":"version1","files":3,"at":1700000000}"#;
        let found: Status = serde_json::from_str(body).unwrap();
        assert_eq!(found.state, State::Uploading);
        assert_eq!(found.files, 3);
    }

    #[test]
    fn claims_only_a_watcher_inside_an_open_folder() {
        let found = status(State::Synced, 0);
        assert!(covers(&found, &[PathBuf::from("/repo")]));
        assert!(!covers(&found, &[PathBuf::from("/elsewhere")]));
    }

    #[test]
    fn treats_a_watcher_that_stopped_beating_as_gone() {
        assert!(is_stale(&status(State::Synced, 0)));
        assert!(!is_stale(&status(State::Synced, now_seconds())));
    }

    #[test]
    fn shows_only_what_is_worth_interrupting_for() {
        assert!(worth_showing(Some(State::Uploading)));
        assert!(worth_showing(Some(State::Failed)));
        assert!(!worth_showing(Some(State::Synced)));
        assert!(!worth_showing(None));
    }

    #[test]
    fn counts_files_in_the_message_it_shows() {
        assert!(describe(&status(State::Uploading, 0)).starts_with("uploading 7 files"));
        let mut one = status(State::Uploading, 0);
        one.files = 1;
        assert!(describe(&one).starts_with("uploading 1 file to"));
        assert!(describe(&status(State::Synced, 0)).is_empty());
    }

    #[test]
    fn puts_the_reason_in_a_failure() {
        let mut failed = status(State::Failed, 0);
        failed.detail = Some("502 Bad Gateway".into());
        assert_eq!(describe(&failed), "upload failed — 502 Bad Gateway");
    }
}
