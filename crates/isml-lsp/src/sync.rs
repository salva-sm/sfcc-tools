//! Uploader state via LSP progress, Zed's only status channel for an extension.
//! Only uploading and failed are shown: a progress item that never ends reads as a stuck spinner.
//! Reads one JSON file per watcher under the uploader's state directory: the layout both programs must agree on.

use crossbeam_channel::{SendError, Sender};
use std::path::{Path, PathBuf};
use std::time::Duration;

use lsp_server::Message;
use lsp_types::notification::{Notification, Progress, ShowMessage};
use lsp_types::request::{Request as RequestTrait, WorkDoneProgressCreate};
use lsp_types::{
    MessageType, NumberOrString, ProgressParams, ProgressParamsValue, ShowMessageParams,
    WorkDoneProgress, WorkDoneProgressBegin, WorkDoneProgressCreateParams, WorkDoneProgressEnd,
    WorkDoneProgressReport,
};

const TOKEN: &str = "sfcc/sync";
const POLL: Duration = Duration::from_millis(1500);
/// A watcher writes on every transition and beats every 20 s; past a minute
/// with no word, it is gone rather than quiet.
const STALE_SECONDS: i64 = 90;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Synced,
    Uploading,
    /// The last upload failed; the changes are still queued.
    Failed,
    Stopped,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct Status {
    pub state: State,
    /// How a workspace recognises its own watcher.
    pub cartridges: String,
    pub hostname: String,
    /// Files in the batch the state refers to.
    #[serde(default)]
    pub files: usize,
    #[serde(default)]
    pub detail: Option<String>,
    /// Seconds since the epoch.
    pub at: i64,
}

/// `upload` in the initialization options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// Start `sfcc-upload` for a workspace with a dw.json and no watcher. Off by default.
    pub autostart: bool,
    /// A notification when an upload fails, and when it recovers. Never for one that works.
    pub notify: bool,
}

impl Options {
    pub fn from_settings(settings: Option<&serde_json::Value>) -> Options {
        let upload = settings.and_then(|settings| settings.get("upload"));
        let flag = |name: &str, default: bool| {
            upload
                .and_then(|upload| upload.get(name))
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(default)
        };
        Options {
            autostart: flag("autostart", false),
            notify: flag("notify", true),
        }
    }
}

pub fn report(roots: Vec<PathBuf>, options: Options, sender: Sender<Message>) {
    std::thread::spawn(move || {
        if create_token(&sender).is_err() {
            return;
        }
        if options.autostart && current(&roots).is_none() {
            for (level, text) in autostart(&roots) {
                if show(&sender, level, text).is_err() {
                    return;
                }
            }
        }
        let mut shown: (Option<State>, String) = (None, String::new());
        let mut failing = false;
        loop {
            let status = current(&roots);
            let now = (
                status.as_ref().map(|status| status.state),
                status.as_ref().map(describe).unwrap_or_default(),
            );
            if now != shown && announce(&sender, shown.0, now.0, now.1.clone()).is_err() {
                return;
            }
            if let Some((level, text)) = toast(status.as_ref(), &mut failing) {
                if options.notify && show(&sender, level, text).is_err() {
                    return;
                }
            }
            shown = now;
            std::thread::sleep(POLL);
        }
    });
}

/// Only the edges: into a failure, and out of it. Retries in between stay in the status bar.
fn toast(status: Option<&Status>, failing: &mut bool) -> Option<(MessageType, String)> {
    let status = match status {
        Some(status) if status.state != State::Stopped => status,
        _ => {
            *failing = false;
            return None;
        }
    };
    match (status.state, *failing) {
        (State::Failed, false) => {
            *failing = true;
            Some((MessageType::ERROR, format!("SFCC {}", describe(status))))
        }
        (State::Synced, true) => {
            *failing = false;
            Some((
                MessageType::INFO,
                format!("SFCC upload back in sync with {}", status.hostname),
            ))
        }
        _ => None,
    }
}

/// `sfcc-upload start` in every root with a dw.json where the uploader looks for one.
/// Says nothing when it started, or was already running.
fn autostart(roots: &[PathBuf]) -> Vec<(MessageType, String)> {
    let mut said = Vec::new();
    for root in roots {
        if !root.join("dw.json").is_file() && !root.join("source").join("dw.json").is_file() {
            continue;
        }
        let mut command = std::process::Command::new("sfcc-upload");
        command
            .arg("start")
            .current_dir(root)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // CREATE_NO_WINDOW: no console flashing up from an editor.
            command.creation_flags(0x0800_0000);
        }
        match command.output() {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                said.push((
                    MessageType::WARNING,
                    "SFCC: upload.autostart is on, but sfcc-upload is not installed".to_string(),
                ));
                break;
            }
            Err(error) => said.push((
                MessageType::WARNING,
                format!("SFCC: could not start sfcc-upload: {error}"),
            )),
            Ok(output) if !output.status.success() => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                if !stderr.contains("already running") {
                    let reason = stderr
                        .lines()
                        .rev()
                        .find(|line| !line.trim().is_empty())
                        .unwrap_or("it exited with an error");
                    said.push((
                        MessageType::WARNING,
                        format!("SFCC: sfcc-upload did not start: {}", reason.trim()),
                    ));
                }
            }
            Ok(_) => {}
        }
    }
    said
}

fn show(
    sender: &Sender<Message>,
    level: MessageType,
    text: String,
) -> Result<(), SendError<Message>> {
    sender.send(Message::Notification(lsp_server::Notification::new(
        ShowMessage::METHOD.to_string(),
        ShowMessageParams {
            typ: level,
            message: text,
        },
    )))
}

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
    Some(root.join("sfcc-upload").join("status"))
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
    fn notifies_only_on_the_way_into_a_failure_and_out_of_it() {
        let mut failing = false;
        let mut failed = status(State::Failed, 0);
        failed.detail = Some("HTTP 503".into());

        assert!(toast(Some(&status(State::Uploading, 0)), &mut failing).is_none());
        assert!(toast(Some(&status(State::Synced, 0)), &mut failing).is_none());

        let (level, text) = toast(Some(&failed), &mut failing).unwrap();
        assert_eq!(level, MessageType::ERROR);
        assert_eq!(text, "SFCC upload failed — HTTP 503");
        // Every retry after it is the same failure.
        assert!(toast(Some(&failed), &mut failing).is_none());
        assert!(toast(Some(&status(State::Uploading, 0)), &mut failing).is_none());
        assert!(toast(Some(&failed), &mut failing).is_none());

        let (level, text) = toast(Some(&status(State::Synced, 0)), &mut failing).unwrap();
        assert_eq!(level, MessageType::INFO);
        assert!(text.contains("back in sync with sbx-001.example.com"));
        assert!(toast(Some(&status(State::Synced, 0)), &mut failing).is_none());
    }

    #[test]
    fn a_watcher_that_went_away_is_not_a_recovery() {
        let mut failing = false;
        toast(Some(&status(State::Failed, 0)), &mut failing);
        assert!(toast(None, &mut failing).is_none());
        assert!(toast(Some(&status(State::Synced, 0)), &mut failing).is_none());
    }

    #[test]
    fn reads_the_upload_options() {
        let on = serde_json::json!({ "upload": { "autostart": true } });
        assert_eq!(
            Options::from_settings(Some(&on)),
            Options {
                autostart: true,
                notify: true
            }
        );
        let quiet = serde_json::json!({ "upload": { "notify": false } });
        assert_eq!(
            Options::from_settings(Some(&quiet)),
            Options {
                autostart: false,
                notify: false
            }
        );
        assert_eq!(
            Options::from_settings(None),
            Options {
                autostart: false,
                notify: true
            }
        );
    }

    #[test]
    fn puts_the_reason_in_a_failure() {
        let mut failed = status(State::Failed, 0);
        failed.detail = Some("502 Bad Gateway".into());
        assert_eq!(describe(&failed), "upload failed — 502 Bad Gateway");
    }
}
