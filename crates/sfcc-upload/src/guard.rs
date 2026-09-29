//! Whether the sandbox still holds what this checkout last sent it.
//!
//! `getetag` changes on every write, even of the same bytes, so a file whose etag is not the one
//! read back after its last upload has been written since by someone else: a colleague on the
//! same code version, Prophet, another checkout. Files sent in bulk have no etag read back - one
//! listing per folder would double a full deploy - and are judged by date instead. The instance
//! ignores `If-Match`, so this is a look before the write, not a lock.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use futures::stream::{self, StreamExt};

use crate::logging;
use crate::manifest::{Entry, Manifest};
use crate::push::Ctx;
use crate::scan::LocalFile;
use crate::webdav::DavEntry;

/// How far the sandbox's clock and this one may disagree.
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
    /// Written on the sandbox by someone else, and left as they are.
    pub held: Vec<String>,
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

    logging::warn(format!(
        "{} file(s) changed on the sandbox since this checkout last uploaded them:",
        found.len()
    ));
    for path in &found {
        logging::warn(format!("  {path}"));
    }
    let ask = overwrite == Overwrite::Ask && interactive();
    if ask && crate::confirm("Overwrite them with your version? [y/N] ")? {
        return Ok(untouched(upserts, removals));
    }

    let held: BTreeSet<&str> = found.iter().map(String::as_str).collect();
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

/// Of `paths` - files, or folders about to be deleted - the recorded files someone else has
/// written since. A file gone from the sandbox is not one.
pub async fn overwritten(ctx: &Ctx, manifest: &Manifest, paths: &[String]) -> Result<Vec<String>> {
    let recorded: BTreeMap<&str, &Entry> = paths
        .iter()
        .flat_map(|path| manifest.under(path))
        .filter(|(_, entry)| entry.etag.is_some() || entry.sent_at.is_some())
        .map(|(path, entry)| (path.as_str(), entry))
        .collect();
    let listings = list_parents(ctx, recorded.keys().copied()).await?;
    Ok(recorded
        .into_iter()
        .filter(|(path, entry)| {
            remote_of(&listings, path).is_some_and(|remote| written_since(entry, remote))
        })
        .map(|(path, _)| path.to_string())
        .collect())
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

fn interactive() -> bool {
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
        // The etag decides, whatever the date says.
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
