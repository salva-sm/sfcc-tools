//! The sandbox log in the debug console, by running `sfcc-upload logger`.

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};

use serde_json::json;

use crate::protocol::Writer;

const LOGGER: &str = "sfcc-upload";
const DEFAULT_LEVELS: &str = "error,customerror";

/// Stops the follower when dropped.
pub struct Logs {
    child: Option<Child>,
}

impl Logs {
    /// Never fails the session: no log is better than no debugger.
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
