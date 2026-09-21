//! The sandbox log, in the debug console.
//!
//! A breakpoint only catches what you predicted. The error that threw
//! somewhere else lands in the instance's log, and having it in the same
//! window is the difference between seeing it and going to look for it.
//!
//! `prost logger` already follows that log and rewrites stack frames into
//! local paths, so this starts it rather than reimplementing it.

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};

use serde_json::json;

use crate::protocol::Writer;

/// The uploader, which is where the log follower lives.
const LOGGER: &str = "prost";
/// A debug session is no place for the whole firehose.
const DEFAULT_LEVELS: &str = "error,customerror";

/// A running log follower, stopped when it goes out of scope.
pub struct Logs {
    child: Option<Child>,
}

impl Logs {
    /// Follow the sandbox log into the debug console. Never fails the
    /// session: a console without the log is worse than one with it, but far
    /// better than no debugger.
    pub fn follow(config: &Path, levels: Option<&str>, writer: &Writer) -> Logs {
        let started = Command::new(LOGGER)
            .args([
                "logger",
                "--color",
                "always",
                "--level",
                levels.unwrap_or(DEFAULT_LEVELS),
                "--config",
                &config.to_string_lossy(),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();

        let mut child = match started {
            Ok(child) => child,
            Err(error) => {
                writer.log(format!(
                    "the sandbox log is not being followed ({LOGGER} could not be started: \
                     {error}). Put it on your PATH, or set \"logs\": false to stop asking."
                ));
                return Logs { child: None };
            }
        };

        if let Some(out) = child.stdout.take() {
            let writer = writer.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(out).lines().map_while(Result::ok) {
                    writer.event(
                        "output",
                        json!({ "category": "stdout", "output": format!("{line}\n") }),
                    );
                }
            });
        }
        Logs { child: Some(child) }
    }

    /// Nothing to follow, because the configuration asked for none.
    pub fn none() -> Logs {
        Logs { child: None }
    }
}

impl Drop for Logs {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
        }
    }
}
