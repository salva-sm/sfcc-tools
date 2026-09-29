//! Pending SFCC error count from log-diff, shown via LSP progress (the only channel Zed renders for an extension).

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

pub use sfcc_core::state::errors::Status;
use sfcc_core::state::{errors, is_within, now_seconds};

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
    for status in errors::all() {
        if !covers(&status, roots) || now_seconds() - status.at > errors::STALE_SECONDS {
            continue;
        }
        if best.as_ref().is_none_or(|found| found.at < status.at) {
            best = Some(status);
        }
    }
    best
}

fn covers(status: &Status, roots: &[PathBuf]) -> bool {
    let checked = Path::new(&status.cartridges);
    roots.iter().any(|root| is_within(checked, root))
}

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
