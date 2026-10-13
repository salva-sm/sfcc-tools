//! One process uploads to a sandbox and code version at a time: two would each keep a
//! manifest and take the other's uploads for someone else's. Who holds the lock is written
//! beside it, since on Windows a locked file cannot be read.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sfcc_core::config::Config;
use sfcc_core::daemon;
use sfcc_core::state::{self, upload};

use crate::logging;

const POLL: Duration = Duration::from_millis(300);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Watcher,
    Push,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Owner {
    pub pid: u32,
    pub kind: Kind,
    pub cartridges: String,
}

impl Owner {
    pub fn describe(&self) -> String {
        match self.kind {
            Kind::Watcher => format!("the watcher of {} (pid {})", self.cartridges, self.pid),
            Kind::Push => format!("a push from {} (pid {})", self.cartridges, self.pid),
        }
    }
}

pub struct Held {
    _file: File,
    owner: PathBuf,
}

impl Drop for Held {
    // Before the file closes, so the lock is never free with a stale owner.
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.owner);
    }
}

pub enum Taken {
    Held(Held),
    /// Nothing when the holder has not said yet who it is.
    By(Option<Owner>),
}

pub fn try_take(config: &Config, kind: Kind) -> Result<Taken> {
    let identity = config.identity();
    let path = upload::lock_path(&identity);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("cannot open {}", path.display()))?;
    match file.try_lock() {
        Ok(()) => {
            let owner = upload::owner_path(&identity);
            let me = Owner {
                pid: std::process::id(),
                kind,
                cartridges: config.cartridges_dir.to_string_lossy().into_owned(),
            };
            state::write(&owner, &me);
            Ok(Taken::Held(Held { _file: file, owner }))
        }
        Err(TryLockError::WouldBlock) => Ok(Taken::By(holder(&identity))),
        Err(TryLockError::Error(error)) => {
            Err(error).with_context(|| format!("cannot lock {}", path.display()))
        }
    }
}

fn holder(identity: &str) -> Option<Owner> {
    state::read::<Owner>(&upload::owner_path(identity)).filter(|owner| daemon::is_alive(owner.pid))
}

/// Waits for a push holding the lock, never for a watcher: it does not end on its own.
pub async fn take(config: &Config, kind: Kind) -> Result<Result<Held, Owner>> {
    let mut said = false;
    loop {
        match try_take(config, kind)? {
            Taken::Held(held) => return Ok(Ok(held)),
            Taken::By(Some(owner)) if owner.kind == Kind::Watcher => return Ok(Err(owner)),
            Taken::By(owner) => {
                if !said {
                    let who =
                        owner.map_or_else(|| "another upload".to_string(), |it| it.describe());
                    logging::info(format!(
                        "{who} is uploading to {} - waiting for it to finish",
                        config.code_version
                    ));
                    said = true;
                }
                tokio::time::sleep(POLL).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sfcc_core::testing::MockDav;

    #[tokio::test]
    async fn one_holder_at_a_time_and_it_says_who_it_is() {
        let server = MockDav::start().await;
        let mut config = server.config();
        config.code_version = format!("lock{}", std::process::id());

        let Taken::Held(held) = try_take(&config, Kind::Watcher).unwrap() else {
            panic!("the lock was free");
        };
        let Taken::By(Some(owner)) = try_take(&config, Kind::Push).unwrap() else {
            panic!("the lock was taken twice");
        };
        assert_eq!(owner.kind, Kind::Watcher);
        assert_eq!(owner.pid, std::process::id());
        assert!(matches!(take(&config, Kind::Push).await, Ok(Err(_))));

        drop(held);
        assert!(!upload::owner_path(&config.identity()).exists());
        assert!(matches!(
            try_take(&config, Kind::Push).unwrap(),
            Taken::Held(_)
        ));
    }
}
