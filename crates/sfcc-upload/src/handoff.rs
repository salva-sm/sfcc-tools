//! A push while a watcher holds the lock is handed to the watcher as a file, and answered the
//! same way: one process uploads, and one manifest is kept.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sfcc_core::config::Config;
use sfcc_core::daemon;
use sfcc_core::state::{self, upload};

use crate::lock::Owner;

const POLL: Duration = Duration::from_millis(250);
const REQUEST: &str = ".request.json";
const REPLY: &str = ".reply.json";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub id: String,
    pub full: bool,
    pub overwrite: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "lowercase")]
pub enum Reply {
    Done {
        uploaded: usize,
        deleted: usize,
        held: Vec<String>,
    },
    Down {
        reason: String,
    },
    Failed {
        error: String,
    },
}

fn dir(config: &Config) -> PathBuf {
    upload::requests_dir(&config.identity())
}

/// Whole or not at all: the other side may look at any moment.
fn put<T: Serialize>(path: PathBuf, value: &T) -> Result<()> {
    let parent = path.parent().context("a request has a folder")?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("cannot create {}", parent.display()))?;
    let partial = path.with_extension("tmp");
    let body = serde_json::to_string(value)?;
    std::fs::write(&partial, body)
        .with_context(|| format!("cannot write {}", partial.display()))?;
    std::fs::rename(&partial, &path).with_context(|| format!("cannot write {}", path.display()))
}

pub fn submit(config: &Config, full: bool, overwrite: bool) -> Result<Request> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or(0);
    let request = Request {
        id: format!("{stamp}-{}", std::process::id()),
        full,
        overwrite,
    };
    put(
        dir(config).join(format!("{}{REQUEST}", request.id)),
        &request,
    )?;
    Ok(request)
}

pub fn take(config: &Config) -> Vec<Request> {
    let Ok(entries) = std::fs::read_dir(dir(config)) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.to_string_lossy().ends_with(REQUEST))
        .collect();
    paths.sort();
    paths
        .into_iter()
        .filter_map(|path| {
            let request = state::read::<Request>(&path);
            let _ = std::fs::remove_file(&path);
            request
        })
        .collect()
}

pub fn answer(config: &Config, request: &Request, reply: &Reply) {
    let _ = put(dir(config).join(format!("{}{REPLY}", request.id)), reply);
}

pub fn clear_replies(config: &Config) {
    let Ok(entries) = std::fs::read_dir(dir(config)) else {
        return;
    };
    for path in entries.flatten().map(|entry| entry.path()) {
        if path.to_string_lossy().ends_with(REPLY) {
            let _ = std::fs::remove_file(path);
        }
    }
}

pub async fn reply(config: &Config, request: &Request, watcher: &Owner) -> Result<Reply> {
    let path = dir(config).join(format!("{}{REPLY}", request.id));
    loop {
        if let Some(reply) = state::read::<Reply>(&path) {
            let _ = std::fs::remove_file(&path);
            return Ok(reply);
        }
        if !daemon::is_alive(watcher.pid) {
            let _ = std::fs::remove_file(dir(config).join(format!("{}{REQUEST}", request.id)));
            bail!("{} stopped before it answered", watcher.describe());
        }
        tokio::time::sleep(POLL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sfcc_core::testing::MockDav;

    #[tokio::test]
    async fn a_request_is_taken_once_and_its_reply_read_once() {
        let server = MockDav::start().await;
        let mut config = server.config();
        config.code_version = format!("handoff{}", std::process::id());

        let request = submit(&config, false, true).unwrap();
        assert_eq!(take(&config), vec![request.clone()]);
        assert!(take(&config).is_empty());

        let done = Reply::Done {
            uploaded: 2,
            deleted: 0,
            held: vec!["app/cartridge/a.js".into()],
        };
        answer(&config, &request, &done);
        let watcher = Owner {
            pid: std::process::id(),
            kind: crate::lock::Kind::Watcher,
            cartridges: "/repo/cartridges".into(),
        };
        assert_eq!(reply(&config, &request, &watcher).await.unwrap(), done);
        let _ = std::fs::remove_dir_all(dir(&config));
    }

    #[tokio::test]
    async fn a_watcher_that_ends_without_answering_is_not_waited_for() {
        let server = MockDav::start().await;
        let mut config = server.config();
        config.code_version = format!("gone{}", std::process::id());
        let request = submit(&config, false, false).unwrap();
        let watcher = Owner {
            pid: u32::MAX,
            kind: crate::lock::Kind::Watcher,
            cartridges: "/repo/cartridges".into(),
        };
        let error = reply(&config, &request, &watcher).await.unwrap_err();
        assert!(format!("{error:#}").contains("stopped before it answered"));
        assert!(take(&config).is_empty());
        let _ = std::fs::remove_dir_all(dir(&config));
    }
}
