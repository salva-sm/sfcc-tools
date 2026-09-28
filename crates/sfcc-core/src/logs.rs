//! The instance log over WebDAV: a mark, and the records written since.

use crate::webdav::{Dav, encode_path};
use anyhow::{Context, Result};
use chrono::{Duration, NaiveDateTime, SecondsFormat, Utc};
use futures::stream::{self, StreamExt, TryStreamExt};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const DEFAULT_LEVELS: &str = "error,customerror,custom";

/// One file per level, app server and day; more at once would only be throttled.
const READS_AT_ONCE: usize = 6;
/// What one request of the log folder reads at most.
const SLICE: u64 = 16 * 1024 * 1024;

/// A timestamped line and every line up to the next one: a stack trace, a request dump.
#[derive(Debug, Clone)]
pub struct Entry {
    pub label: String,
    /// `2026-09-09 07:26:29.103 GMT`, or empty for lines that arrived without one.
    pub moment: String,
    pub lines: Vec<String>,
}

impl Entry {
    pub fn moment_utc(&self) -> Option<chrono::DateTime<Utc>> {
        let bare = self.moment.trim_end_matches(" GMT");
        NaiveDateTime::parse_from_str(bare, "%Y-%m-%d %H:%M:%S%.f")
            .ok()
            .map(|naive| naive.and_utc())
    }
}

/// Where the log ended at some moment: the length of every file of the wanted levels.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Mark {
    pub taken: String,
    /// `YYYYMMDD`; files of earlier days are finished history and not read.
    #[serde(default)]
    pub day: String,
    /// Sorted, so a mark kept under version control changes only where the log did.
    pub offsets: BTreeMap<String, u64>,
}

impl Mark {
    pub fn start_of_today() -> Mark {
        Mark::days_back(0)
    }

    /// Older files may be gone, or moved to `log_archive`, which is not read.
    pub fn days_back(days: u32) -> Mark {
        let day = Utc::now() - Duration::days(i64::from(days));
        Mark {
            taken: now(),
            day: day.format("%Y%m%d").to_string(),
            offsets: BTreeMap::new(),
        }
    }
}

#[derive(Debug)]
pub struct Since {
    pub entries: Vec<Entry>,
    pub next: Mark,
}

pub async fn mark(dav: &Dav, levels: &[String]) -> Result<Mark> {
    let day = today();
    let offsets = listing(dav)
        .await?
        .into_iter()
        .filter(|file| is_wanted(&file.name, levels, &day))
        .map(|file| (file.name, file.size))
        .collect();
    Ok(Mark {
        taken: now(),
        day,
        offsets,
    })
}

/// A file the mark does not know counts whole; one shorter than its offset was
/// rotated. A trailing line still being written is left for the next read.
pub async fn since(dav: &Dav, mark: &Mark, levels: &[String]) -> Result<Since> {
    let mut entries = Vec::new();
    let next = since_each(dav, mark, levels, |entry| entries.push(entry)).await?;
    order(&mut entries);
    Ok(Since { entries, next })
}

/// [`since`] an entry at a time, a file after another, each read in slices of [`SLICE`]: a
/// day of an instance's log - what a first read or a Monday takes in - never has to fit in memory.
pub async fn since_each(
    dav: &Dav,
    mark: &Mark,
    levels: &[String],
    mut each: impl FnMut(Entry),
) -> Result<Mark> {
    let today = today();
    let first_day = match mark.day.is_empty() {
        true => today.as_str(),
        false => mark.day.as_str(),
    };

    let wanted: Vec<_> = listing(dav)
        .await?
        .into_iter()
        .filter_map(|file| {
            let day = file_day(&file.name)?.to_string();
            (day.as_str() >= first_day && is_wanted(&file.name, levels, &day))
                .then_some((file, day))
        })
        .collect();

    let mut offsets = BTreeMap::new();
    for (file, day) in wanted {
        let mut offset = mark.offsets.get(&file.name).copied().unwrap_or(0);
        if file.size < offset {
            offset = 0;
        }
        let url = format!("{}/{}", dav.logs_url(), encode_path(&file.name));
        let mut parser = EntryParser::new(&file.name);
        // Bytes past the last complete line: the start of the next slice's first line.
        let mut carry: Vec<u8> = Vec::new();
        let mut fetched = offset;
        while fetched < file.size {
            let last = (fetched + SLICE).min(file.size) - 1;
            let bytes = dav
                .read_range(&url, fetched, last)
                .await
                .with_context(|| format!("cannot read {}", file.name))?;
            if bytes.is_empty() {
                break;
            }
            fetched += bytes.len() as u64;
            carry.extend_from_slice(&bytes);
            if let Some(end) = carry.iter().rposition(|byte| *byte == b'\n') {
                let complete: Vec<u8> = carry.drain(..=end).collect();
                offset += complete.len() as u64;
                for line in String::from_utf8_lossy(&complete).lines() {
                    if let Some(entry) = parser.line(line) {
                        each(entry);
                    }
                }
            }
        }
        if let Some(entry) = parser.finish() {
            each(entry);
        }
        // Yesterday's files are done; keeping their offsets would only grow the mark.
        if day == today {
            offsets.insert(file.name, offset);
        }
    }

    Ok(Mark {
        taken: now(),
        day: today,
        offsets,
    })
}

/// A file in log_archive, as [`archive_files`] lists it and [`read_archived`] reads it.
#[derive(Debug, Clone)]
pub struct Archived {
    pub url: String,
    pub name: String,
}

/// Days still in the log folder are left to [`since`]. Reads the archive whole:
/// for a baseline, not every run. Instances gzip some levels there and leave others
/// (customerror, customwarn) as they were.
pub async fn archived(dav: &Dav, first_day: &str, levels: &[String]) -> Result<Vec<Entry>> {
    let files = archive_files(dav, first_day, levels).await?;
    let read: Vec<Vec<Entry>> = stream::iter(files.iter().map(|file| read_archived(dav, file)))
        .buffered(READS_AT_ONCE)
        .try_collect()
        .await?;
    let mut entries: Vec<Entry> = read.into_iter().flatten().collect();
    order(&mut entries);
    Ok(entries)
}

/// The files of log_archive from `first_day` on, oldest day first, for reading one at a time:
/// a month of an instance's archive does not fit in memory at once.
pub async fn archive_files(dav: &Dav, first_day: &str, levels: &[String]) -> Result<Vec<Archived>> {
    let live: std::collections::HashSet<String> = listing(dav)
        .await?
        .into_iter()
        .map(|file| file.name)
        .collect();

    let archive = format!("{}/log_archive", dav.logs_url());
    let mut files = Vec::new();
    // No archive is a 404, and an empty list; any other failure would quietly shorten the history.
    let listed = dav
        .list(&archive)
        .await
        .context("cannot list log_archive")?;
    for entry in listed {
        match entry.is_dir {
            // Some instances keep a folder per day or per month inside.
            true => {
                let folder = format!("{archive}/{}", encode_path(&entry.name));
                let inside = dav
                    .list(&folder)
                    .await
                    .with_context(|| format!("cannot list log_archive/{}", entry.name))?;
                for file in inside {
                    if !file.is_dir {
                        files.push((format!("{folder}/{}", encode_path(&file.name)), file.name));
                    }
                }
            }
            false => files.push((
                format!("{archive}/{}", encode_path(&entry.name)),
                entry.name,
            )),
        }
    }

    let mut wanted: Vec<Archived> = files
        .into_iter()
        .filter(|(_, name)| {
            let plain = name.strip_suffix(".gz").unwrap_or(name);
            file_day(plain).is_some_and(|day| {
                day >= first_day && !live.contains(plain) && is_wanted(plain, levels, day)
            })
        })
        .map(|(url, name)| Archived { url, name })
        .collect();
    let day_of = |file: &Archived| {
        file_day(file.name.strip_suffix(".gz").unwrap_or(&file.name)).map(str::to_string)
    };
    wanted.sort_by(|left, right| {
        day_of(left)
            .cmp(&day_of(right))
            .then(left.name.cmp(&right.name))
    });
    Ok(wanted)
}

pub async fn read_archived(dav: &Dav, file: &Archived) -> Result<Vec<Entry>> {
    let mut entries = Vec::new();
    read_archived_each(dav, file, |entry| entries.push(entry)).await?;
    Ok(entries)
}

/// Each entry of an archived file in turn, never the file whole: a day of an instance's
/// warnings can take more memory than there is once uncompressed.
pub async fn read_archived_each(
    dav: &Dav,
    file: &Archived,
    mut each: impl FnMut(Entry),
) -> Result<()> {
    use std::io::BufRead;
    let name = &file.name;
    let bytes = dav
        .read_bytes(&file.url)
        .await
        .with_context(|| format!("cannot read {name}"))?;
    let (plain, mut reader): (&str, Box<dyn BufRead>) = match name.strip_suffix(".gz") {
        Some(plain) => (
            plain,
            Box::new(std::io::BufReader::new(flate2::read::MultiGzDecoder::new(
                bytes.as_slice(),
            ))),
        ),
        None => (name.as_str(), Box::new(bytes.as_slice())),
    };
    let mut parser = EntryParser::new(plain);
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = reader.read_until(b'\n', &mut line);
        if !line.is_empty() {
            let text = String::from_utf8_lossy(&line);
            if let Some(entry) = parser.line(text.trim_end_matches(['\n', '\r'])) {
                each(entry);
            }
        }
        match read {
            Ok(0) => break,
            Ok(_) => {}
            // SFCC leaves an archive cut short now and then: what it holds still counts.
            Err(error) => {
                eprintln!("warning: {name} is cut short ({error}): read as far as it goes");
                break;
            }
        }
    }
    if let Some(entry) = parser.finish() {
        each(entry);
    }
    Ok(())
}

async fn listing(dav: &Dav) -> Result<Vec<crate::webdav::DavEntry>> {
    let files = dav
        .list(dav.logs_url())
        .await
        .context("cannot list the instance logs")?;
    Ok(files.into_iter().filter(|file| !file.is_dir).collect())
}

/// The instance runs on GMT, and so do its file names.
pub fn today() -> String {
    Utc::now().format("%Y%m%d").to_string()
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// `error-blade1-4-appserver-20260905.log` -> `20260905`.
pub fn file_day(name: &str) -> Option<&str> {
    let stem = name.strip_suffix(".log")?;
    let day = stem.rsplit('-').next()?;
    (day.len() == 8 && day.bytes().all(|byte| byte.is_ascii_digit())).then_some(day)
}

pub fn parse_levels(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|level| level.trim().to_lowercase())
        .filter(|level| !level.is_empty())
        .collect()
}

pub fn has_level(levels: &[String], label: &str) -> bool {
    levels.iter().any(|level| level == "all" || level == label)
}

pub fn is_wanted(name: &str, levels: &[String], day: &str) -> bool {
    if !name.ends_with(".log") || !name.contains(day) {
        return false;
    }
    levels
        .iter()
        .any(|level| level == "all" || name.starts_with(level.as_str()))
}

pub fn parse_entries(file: &str, text: &str) -> Vec<Entry> {
    let mut parser = EntryParser::new(file);
    let mut entries: Vec<Entry> = text.lines().filter_map(|line| parser.line(line)).collect();
    entries.extend(parser.finish());
    entries
}

/// Lines into entries as they come: each entry is handed over once the next one starts.
pub struct EntryParser {
    label: String,
    current: Option<Entry>,
}

impl EntryParser {
    pub fn new(file: &str) -> EntryParser {
        EntryParser {
            label: file.split('-').next().unwrap_or(file).to_string(),
            current: None,
        }
    }

    /// The entry this line closes, when it starts the next one.
    pub fn line(&mut self, line: &str) -> Option<Entry> {
        if line.trim().is_empty() {
            return None;
        }
        match (moment(line), self.current.as_mut()) {
            (None, Some(entry)) => {
                entry.lines.push(line.to_string());
                None
            }
            (moment, _) => self.current.replace(Entry {
                label: self.label.clone(),
                moment: moment.unwrap_or_default(),
                lines: vec![line.to_string()],
            }),
        }
    }

    pub fn finish(self) -> Option<Entry> {
        self.current
    }
}

/// Leftovers of an entry from an earlier read carry no moment and stay in front.
pub fn order(batch: &mut [Entry]) {
    batch.sort_by(|left, right| left.moment.cmp(&right.moment));
}

/// `[2026-09-09 07:26:29.103 GMT]`
fn moment(line: &str) -> Option<String> {
    let inner = line.strip_prefix('[')?.split_once(']')?.0;
    let shape = inner.as_bytes();
    if shape.len() < 19 || shape[4] != b'-' || shape[7] != b'-' || shape[13] != b':' {
        return None;
    }
    Some(inner.to_string())
}

#[cfg(test)]
#[path = "logs_tests.rs"]
mod tests;
