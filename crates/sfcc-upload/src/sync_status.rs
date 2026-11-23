//! The watcher's state as a file an editor can poll, since a detached watcher
//! is otherwise invisible and a failed upload looks like one that worked.

use std::path::PathBuf;

use sfcc_core::config::Config;
pub use sfcc_core::state::upload::{Conflict, State, Status};
use sfcc_core::state::{self, now_seconds, upload};

pub fn status_path(config: &Config) -> PathBuf {
    upload::path(&config.identity())
}

fn new(config: &Config, state: State) -> Status {
    Status {
        state,
        cartridges: config.cartridges_dir.to_string_lossy().into_owned(),
        hostname: config.hostname.clone(),
        code_version: config.code_version.clone(),
        files: 0,
        detail: None,
        held: Vec::new(),
        down: None,
        at: now_seconds(),
    }
}

pub fn publish(config: &Config, state: State) {
    state::write(&status_path(config), &new(config, state));
}

pub fn publish_uploading(config: &Config, files: usize) {
    let status = Status {
        files,
        ..new(config, State::Uploading)
    };
    state::write(&status_path(config), &status);
}

pub fn publish_failure(config: &Config, detail: String) {
    let status = Status {
        detail: Some(detail),
        ..new(config, State::Failed)
    };
    state::write(&status_path(config), &status);
}

pub fn publish_held(config: &Config, held: Vec<Conflict>, detail: String) {
    let status = Status {
        files: held.len(),
        detail: Some(detail),
        held,
        ..new(config, State::Held)
    };
    state::write(&status_path(config), &status);
}

pub fn publish_down(config: &Config, detail: String, reason: String) {
    let status = Status {
        detail: Some(detail),
        down: Some(reason),
        ..new(config, State::Failed)
    };
    state::write(&status_path(config), &status);
}

/// Whoever stops a watcher leaves the row as stopped, so the TUI can see it and
/// start it again instead of being left with nothing to act on.
pub fn stopped(config: &Config) {
    publish(config, State::Stopped);
}

/// The same by identity, for `stop --all`: the existing row keeps its sandbox
/// and checkout and only turns stopped.
pub fn stopped_identity(identity: &str) {
    if let Some(status) = state::read::<Status>(&upload::path(identity)) {
        state::write(
            &upload::path(identity),
            &Status {
                state: State::Stopped,
                files: 0,
                detail: None,
                held: Vec::new(),
                down: None,
                at: now_seconds(),
                ..status
            },
        );
    }
}

/// Wipe the row outright. Only the tests need this now; stopping leaves it.
#[cfg(test)]
pub fn clear(config: &Config) {
    let _ = std::fs::remove_file(status_path(config));
}
