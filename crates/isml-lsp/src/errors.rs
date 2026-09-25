//! How many SFCC errors are waiting to be looked at, in Zed's status bar.
//!
//! `log-diff check` and `log-diff watch` write, per sandbox, how many of its
//! error signatures are pending - reported and not dealt with yet. A pending
//! error is worth interrupting for, so while there is one this shows it
//! through LSP progress, the one channel Zed renders for an extension, the
//! same way the uploader's state is shown; with none, it shows nothing.
//!
//! # The file it reads
//!
//! One JSON file per sandbox under log-diff's own folder - the one layout the
//! two programs have to agree on.

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

const TOKEN: &str = "sfcc/errors";
const POLL: Duration = Duration::from_secs(5);
/// A check a day old says nothing about now; `watch` writes every few seconds.
const STALE_SECONDS: i64 = 24 * 60 * 60;

/// What log-diff last found for one sandbox.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct Status {
    /// The cartridges of the checkout it was run for.
    pub cartridges: String,
    /// The sandbox.
    pub hostname: String,
    /// Signatures reported and not dealt with.
    pub pending: usize,
    /// Of them, the ones the last pass turned up.
    #[serde(default)]
    pub new: usize,
    /// Seconds since the epoch.
    pub at: i64,
}

/// Follow log-diff in the background and report to the editor.
pub fn report(roots: Vec<PathBuf>, sender: Sender<Message>) {
    std::thread::spawn(move || {
        if create_token(&sender).is_err() {
            return;
        }
        let mut shown: Option<String> = None;
        loop {
            let message = current(&roots).and_then(|status| describe(&status));
            if message != shown && announce(&sender, shown.is_some(), message.clone()).is_err() {
                return;
            }
            shown = message;
            std::thread::sleep(POLL);
        }
    });
}

/// The freshest status for a sandbox whose checkout is one of the open folders.
pub fn current(roots: &[PathBuf]) -> Option<Status> {
    let mut best: Option<Status> = None;
    for entry in std::fs::read_dir(status_dir()?).ok()?.flatten() {
        let Some(status) = read(&entry.path()) else {
            continue;
        };
        if !covers(&status, roots) || now_seconds() - status.at > STALE_SECONDS {
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

fn covers(status: &Status, roots: &[PathBuf]) -> bool {
    let checked = PathBuf::from(&status.cartridges);
    roots.iter().any(|root| checked.starts_with(root))
}

/// log-diff's folder, as it derives it.
fn status_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        if let Ok(appdata) = std::env::var("APPDATA") {
            return Some(PathBuf::from(appdata).join("log-diff").join("status"));
        }
    }
    let root = match std::env::var("XDG_CONFIG_HOME") {
        Ok(config) if !config.is_empty() => PathBuf::from(config),
        _ => PathBuf::from(std::env::var("HOME").ok()?).join(".config"),
    };
    Some(root.join("log-diff").join("status"))
}

/// `2 SFCC errors pending (1 new)`, or nothing when none is.
pub fn describe(status: &Status) -> Option<String> {
    let noun = if status.pending == 1 {
        "error"
    } else {
        "errors"
    };
    match (status.pending, status.new) {
        (0, _) => None,
        (pending, 0) => Some(format!("{pending} SFCC {noun} pending - log-diff list")),
        (pending, new) => Some(format!(
            "{pending} SFCC {noun} pending ({new} new) - log-diff list"
        )),
    }
}

fn announce(
    sender: &Sender<Message>,
    was_shown: bool,
    message: Option<String>,
) -> Result<(), SendError<Message>> {
    let value = match (was_shown, message) {
        (false, Some(message)) => WorkDoneProgress::Begin(WorkDoneProgressBegin {
            title: "SFCC".to_string(),
            message: Some(message),
            ..Default::default()
        }),
        (true, Some(message)) => WorkDoneProgress::Report(WorkDoneProgressReport {
            message: Some(message),
            ..Default::default()
        }),
        (true, None) => WorkDoneProgress::End(WorkDoneProgressEnd { message: None }),
        (false, None) => return Ok(()),
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

    fn status(pending: usize, new: usize) -> Status {
        Status {
            cartridges: "/repo/source/cartridges".into(),
            hostname: "sbx-001.example.com".into(),
            pending,
            new,
            at: 0,
        }
    }

    #[test]
    fn reads_what_log_diff_writes() {
        let found: Status = serde_json::from_str(
            r#"{"cartridges":"/repo/cartridges","hostname":"h","pending":2,"new":1,"at":1700000000}"#,
        )
        .unwrap();
        assert_eq!(found.pending, 2);
        assert_eq!(found.new, 1);
    }

    #[test]
    fn says_something_only_while_something_is_pending() {
        assert_eq!(describe(&status(0, 0)), None);
        assert_eq!(
            describe(&status(1, 0)).as_deref(),
            Some("1 SFCC error pending - log-diff list")
        );
        assert_eq!(
            describe(&status(3, 2)).as_deref(),
            Some("3 SFCC errors pending (2 new) - log-diff list")
        );
    }

    #[test]
    fn claims_only_a_checkout_inside_an_open_folder() {
        assert!(covers(&status(1, 0), &[PathBuf::from("/repo")]));
        assert!(!covers(&status(1, 0), &[PathBuf::from("/elsewhere")]));
    }
}
