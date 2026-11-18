//! Whether another writer touched a file since this checkout last sent it. `getetag` changes on
//! every write; files sent in bulk get no etag read back and are judged by date. Either check
//! only flags a suspect: a file is held as another writer's only once its bytes no longer hash to
//! what this checkout last sent, which keeps a clock skew or an identical re-save from reading
//! as a conflict. The instance ignores `If-Match`, so this is a check before the write, not a lock.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use futures::stream::{self, StreamExt};
use sfcc_core::state::upload::Conflict;

use crate::logging;
use crate::manifest::{Entry, Manifest};
use crate::push::Ctx;
use crate::scan::LocalFile;
use crate::webdav::DavEntry;

const CLOCK_SLACK_SECONDS: i64 = 120;
/// Past this many folders, reading etags back costs more than the upload did.
const REMEMBER_FOLDERS: usize = 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overwrite {
    /// At a terminal; anywhere else, the same as `Never`.
    Ask,
    Never,
    Always,
}

pub struct Settled {
    pub upserts: Vec<LocalFile>,
    pub removals: Vec<String>,
    /// Left as the sandbox has them.
    pub held: Vec<Conflict>,
}

/// With one uploader per code version on this machine (lock.rs), a change this uploader did
/// not make came from Prophet, a teammate or another machine - WebDAV names no author.
pub fn by_another_writer(count: usize) -> String {
    format!(
        "{count} file(s) changed on the sandbox by another writer - Prophet, a teammate, or \
         another machine"
    )
}

pub const OVERWRITE_HINT: &str = "`sfcc-upload push --overwrite` replaces them";

/// Shared by the direct push and the handed-over reply.
pub fn warn_held(held: &[Conflict]) {
    logging::warn(format!("{}:", by_another_writer(held.len())));
    for conflict in held {
        logging::warn(format!("  {}", conflict.path));
    }
}

pub async fn settle(
    ctx: &Ctx,
    manifest: &Manifest,
    upserts: Vec<LocalFile>,
    removals: Vec<String>,
    overwrite: Overwrite,
) -> Result<Settled> {
    let untouched = |upserts, removals| Settled {
        upserts,
        removals,
        held: Vec::new(),
    };
    if overwrite == Overwrite::Always || (upserts.is_empty() && removals.is_empty()) {
        return Ok(untouched(upserts, removals));
    }
    let paths: Vec<String> = upserts
        .iter()
        .map(|file| file.relative.clone())
        .chain(removals.iter().cloned())
        .collect();
    let found = overwritten(ctx, manifest, &paths).await?;
    if found.is_empty() {
        return Ok(untouched(upserts, removals));
    }

    warn_held(&found);
    let ask = overwrite == Overwrite::Ask && interactive();
    if ask && crate::confirm("Overwrite them with your version? [y/N] ")? {
        return Ok(untouched(upserts, removals));
    }

    let held: BTreeSet<&str> = found
        .iter()
        .map(|conflict| conflict.path.as_str())
        .collect();
    let holds = |path: &str| {
        held.iter()
            .any(|file| *file == path || file.starts_with(&format!("{path}/")))
    };
    Ok(Settled {
        upserts: upserts
            .into_iter()
            .filter(|file| !holds(&file.relative))
            .collect(),
        removals: removals.into_iter().filter(|path| !holds(path)).collect(),
        held: found,
    })
}

/// What an etag or date flagged before its content is read back.
struct Suspect {
    path: String,
    /// What this checkout last sent, to tell its own write from another's.
    hash: u64,
    /// The sandbox copy's last-modified, carried so a confirmed conflict can show it.
    modified: String,
}

/// `paths` may be folders about to be deleted. A file gone from the sandbox is not a conflict.
pub async fn overwritten(
    ctx: &Ctx,
    manifest: &Manifest,
    paths: &[String],
) -> Result<Vec<Conflict>> {
    let recorded: BTreeMap<&str, &Entry> = paths
        .iter()
        .flat_map(|path| manifest.under(path))
        .filter(|(_, entry)| entry.etag.is_some() || entry.sent_at.is_some())
        .map(|(path, entry)| (path.as_str(), entry))
        .collect();
    let listings = list_parents(ctx, recorded.keys().copied()).await?;
    let suspect: Vec<Suspect> = recorded
        .into_iter()
        .filter_map(|(path, entry)| {
            let remote = remote_of(&listings, path)?;
            written_since(entry, remote).then(|| Suspect {
                path: path.to_string(),
                hash: entry.hash,
                modified: remote.modified.clone(),
            })
        })
        .collect();
    confirmed_conflicts(ctx, suspect).await
}

/// A suspect whose bytes still hash to what this checkout last sent is nobody else's write,
/// whatever its etag or date said: confirming by content makes a clock skew or an identical
/// re-save transparent. Only suspects are read, in parallel, so a clean batch costs no reads.
async fn confirmed_conflicts(ctx: &Ctx, suspect: Vec<Suspect>) -> Result<Vec<Conflict>> {
    let checked: Vec<(Suspect, bool)> = stream::iter(suspect)
        .map(|suspect| async move {
            let ours = still_ours(ctx, &suspect.path, suspect.hash).await;
            (suspect, ours)
        })
        .buffer_unordered(ctx.jobs)
        .collect()
        .await;
    Ok(checked
        .into_iter()
        .filter(|(_, ours)| !ours)
        .map(|(suspect, _)| Conflict {
            path: suspect.path,
            sandbox_at: modified_seconds(&suspect.modified),
        })
        .collect())
}

/// A read that fails counts as not ours: better to hold a file than to overwrite another
/// writer's work unseen.
async fn still_ours(ctx: &Ctx, path: &str, sent_hash: u64) -> bool {
    match ctx.dav.read_bytes(&ctx.dav.file_url(path)).await {
        Ok(bytes) => xxhash_rust::xxh3::xxh3_64(&bytes) == sent_hash,
        Err(_) => false,
    }
}

pub fn written_since(entry: &Entry, remote: &DavEntry) -> bool {
    match (&entry.etag, entry.sent_at) {
        (Some(etag), _) => !remote.etag.is_empty() && remote.etag != *etag,
        (None, Some(sent)) => modified_seconds(&remote.modified)
            .is_some_and(|modified| modified > sent + CLOCK_SLACK_SECONDS),
        (None, None) => false,
    }
}

/// Never fails the upload it follows: without an etag, the next check goes by date.
pub async fn remember(ctx: &Ctx, manifest: &mut Manifest, paths: &[String]) {
    let folders: BTreeSet<&str> = paths.iter().map(|path| parent(path)).collect();
    if paths.is_empty() || folders.len() > REMEMBER_FOLDERS {
        return;
    }
    let Ok(listings) = list_parents(ctx, paths.iter().map(String::as_str)).await else {
        return;
    };
    for path in paths {
        let remote = remote_of(&listings, path).filter(|remote| !remote.etag.is_empty());
        if let (Some(remote), Some(entry)) = (remote, manifest.files.get_mut(path)) {
            entry.etag = Some(remote.etag.clone());
        }
    }
}

type Listings = BTreeMap<String, Vec<DavEntry>>;

async fn list_parents<'a>(ctx: &Ctx, paths: impl Iterator<Item = &'a str>) -> Result<Listings> {
    let folders: BTreeSet<&str> = paths.map(parent).collect();
    let listed: Vec<(String, Result<Vec<DavEntry>>)> = stream::iter(folders)
        .map(|folder| async move {
            let listing = ctx.dav.list(&ctx.dav.file_url(folder)).await;
            (folder.to_string(), listing)
        })
        .buffer_unordered(ctx.jobs)
        .collect()
        .await;
    listed
        .into_iter()
        .map(|(folder, listing)| Ok((folder, listing?)))
        .collect()
}

fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(folder, _)| folder)
}

fn remote_of<'a>(listings: &'a Listings, path: &str) -> Option<&'a DavEntry> {
    let (folder, name) = path.rsplit_once('/')?;
    listings
        .get(folder)?
        .iter()
        .find(|entry| !entry.is_dir && entry.name == name)
}

fn modified_seconds(text: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc2822(text)
        .ok()
        .map(|moment| moment.timestamp())
}

pub fn interactive() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote(etag: &str, modified: &str) -> DavEntry {
        DavEntry {
            name: "a.js".to_string(),
            is_dir: false,
            size: 1,
            modified: modified.to_string(),
            etag: etag.to_string(),
        }
    }

    fn entry(etag: Option<&str>, sent_at: Option<i64>) -> Entry {
        Entry {
            etag: etag.map(str::to_string),
            sent_at,
            ..Entry::default()
        }
    }

    const MODIFIED: &str = "Tue, 29 Sep 2026 07:01:48 GMT";
    const AT: i64 = 1_790_665_308;

    #[test]
    fn an_etag_other_than_ours_is_someone_elses_write() {
        assert!(written_since(
            &entry(Some("a1"), None),
            &remote("b2", MODIFIED)
        ));
        assert!(!written_since(
            &entry(Some("a1"), None),
            &remote("a1", MODIFIED)
        ));
        assert!(!written_since(
            &entry(Some("a1"), Some(0)),
            &remote("a1", MODIFIED)
        ));
    }

    #[test]
    fn without_an_etag_only_a_write_well_after_ours_counts() {
        assert_eq!(modified_seconds(MODIFIED), Some(AT));
        assert!(written_since(
            &entry(None, Some(AT - 600)),
            &remote("x", MODIFIED)
        ));
        assert!(!written_since(
            &entry(None, Some(AT - 60)),
            &remote("x", MODIFIED)
        ));
        assert!(!written_since(&entry(None, None), &remote("x", MODIFIED)));
    }

    #[test]
    fn a_server_without_etags_never_reads_as_a_conflict() {
        assert!(!written_since(
            &entry(Some("a1"), None),
            &remote("", MODIFIED)
        ));
    }
}
