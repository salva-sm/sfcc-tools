//! The watcher's state as a file an editor can poll, since a detached watcher
//! is otherwise invisible and a failed upload looks like one that worked.

use std::path::PathBuf;

use sfcc_core::config::Config;
pub use sfcc_core::state::upload::{State, Status};
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

pub fn clear(config: &Config) {
    let _ = std::fs::remove_file(status_path(config));
}
