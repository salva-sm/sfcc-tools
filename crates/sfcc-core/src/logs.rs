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
/// Read at a record's offset to take it whole: a stack and a request dump fit many times over.
const RECORD_BYTES: u64 = 256 * 1024;

/// A timestamped line and every line up to the next one: a stack trace, a request dump.
#[derive(Debug, Clone)]
pub struct Entry {
    pub label: String,
    /// `2026-09-09 07:26:29.103 GMT`, or empty for lines that arrived without one.
    pub moment: String,
    pub lines: Vec<String>,
    /// `error-blade1-4-appserver-20260905.log`, the plain name even when it was read gzipped.
    pub file: String,
    /// Where its first line starts in the file; only known in the log folder, where files are
    /// read as they are, not in log_archive.
    pub offset: Option<u64>,
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
    let (next, _) = since_each(dav, mark, levels, |entry| entries.push(entry)).await?;
    order(&mut entries);
    Ok(Since { entries, next })
}

/// What a read took: to tell a slow instance from a log read again from the start.
#[derive(Debug, Clone, Default)]
pub struct Reading {
    pub files: usize,
    /// Files the mark did not know, or that were rotated: read from their first byte.
    pub whole: usize,
    pub bytes: u64,
    pub requests: usize,
    pub elapsed: std::time::Duration,
}

impl std::fmt::Display for Reading {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let megabytes = self.bytes as f64 / 1_048_576.0;
        let seconds = self.elapsed.as_secs_f64().max(0.001);
        write!(
            out,
            "read {megabytes:.1} MB from {} file(s) ({} from the start) in {} request(s), {seconds:.1}s, {:.1} MB/s",
            self.files,
            self.whole,
            self.requests,
            megabytes / seconds
        )
    }
}

/// [`since`] an entry at a time, each file read in slices of [`SLICE`]: a day of an
/// instance's log - what a first read or a Monday takes in - never has to fit in memory.
/// [`READS_AT_ONCE`] files download at once; their entries come a slice at a time, in order
/// within each file.
pub async fn since_each(
    dav: &Dav,
    mark: &Mark,
    levels: &[String],
    mut each: impl FnMut(Entry),
) -> Result<(Mark, Reading)> {
    let started = std::time::Instant::now();
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

    struct Open {
        name: String,
        day: String,
        parser: EntryParser,
        /// Bytes past the last complete line: the start of the next slice's first line.
        carry: Vec<u8>,
        offset: u64,
    }
    let mut reading = Reading {
        files: wanted.len(),
        ..Reading::default()
    };
    let mut open: Vec<Option<Open>> = Vec::new();
    let mut plan = Vec::new();
    for (index, (file, day)) in wanted.into_iter().enumerate() {
        let mut offset = mark.offsets.get(&file.name).copied().unwrap_or(0);
        if file.size < offset {
            offset = 0;
        }
        if offset == 0 && file.size > 0 {
            reading.whole += 1;
        }
        let url = format!("{}/{}", dav.logs_url(), encode_path(&file.name));
        plan.push((index, url, file.name.clone(), offset, file.size));
        open.push(Some(Open {
            parser: EntryParser::new(&file.name),
            name: file.name,
            day,
            carry: Vec::new(),
            offset,
        }));
    }

    // A slice of a file, or `None` once the file is done.
    let (sender, mut received) =
        tokio::sync::mpsc::channel::<(usize, Option<Vec<u8>>)>(READS_AT_ONCE * 2);
    let download = async move {
        stream::iter(plan.into_iter().map(Ok::<_, anyhow::Error>))
            .try_for_each_concurrent(READS_AT_ONCE, |(index, url, name, offset, size)| {
                let sender = sender.clone();
                async move {
                    let mut fetched = offset;
                    while fetched < size {
                        let last = (fetched + SLICE).min(size) - 1;
                        let bytes = dav
                            .read_range(&url, fetched, last)
                            .await
                            .with_context(|| format!("cannot read {name}"))?;
                        if bytes.is_empty() {
                            break;
                        }
                        fetched += bytes.len() as u64;
                        // Nobody listens only once the reading below failed, which it cannot.
                        let _ = sender.send((index, Some(bytes))).await;
                    }
                    let _ = sender.send((index, None)).await;
                    Ok(())
                }
            })
            .await
    };
    let parse = async {
        let mut offsets = BTreeMap::new();
        while let Some((index, slice)) = received.recv().await {
            let Some(bytes) = slice else {
                let Some(file) = open[index].take() else {
                    continue;
                };
                if let Some(entry) = file.parser.finish() {
                    each(entry);
                }
                // Yesterday's files are done; keeping their offsets would only grow the mark.
                if file.day == today {
                    offsets.insert(file.name, file.offset);
                }
                continue;
            };
            reading.requests += 1;
            reading.bytes += bytes.len() as u64;
            let Some(file) = open[index].as_mut() else {
                continue;
            };
            file.carry.extend_from_slice(&bytes);
            if let Some(end) = file.carry.iter().rposition(|byte| *byte == b'\n') {
                let complete: Vec<u8> = file.carry.drain(..=end).collect();
                let at = file.offset;
                file.offset += complete.len() as u64;
                parse_lines(&mut file.parser, &complete, at, &mut each);
            }
        }
        offsets
    };
    let (downloaded, offsets) = futures::join!(download, parse);
    downloaded?;
    reading.elapsed = started.elapsed();

    Ok((
        Mark {
            taken: now(),
            day: today,
            offsets,
        },
        reading,
    ))
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

/// Complete lines that start at `offset` of a file, into its parser.
fn parse_lines(parser: &mut EntryParser, bytes: &[u8], offset: u64, each: &mut impl FnMut(Entry)) {
    let mut at = offset;
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        let text = String::from_utf8_lossy(line);
        if let Some(entry) = parser.line_at(text.trim_end_matches(['\n', '\r']), at) {
            each(entry);
        }
        at += line.len() as u64;
    }
}

/// The record logged at `moment` in `file` that `wanted` picks, wherever the instance keeps it
/// now: at `offset` in the log folder, anywhere else in that file, or in log_archive once it
/// has been moved there. `None` once the instance keeps it no longer.
pub async fn record(
    dav: &Dav,
    file: &str,
    offset: Option<u64>,
    moment: &str,
    wanted: impl Fn(&Entry) -> bool,
) -> Result<Option<Entry>> {
    let wanted = |entry: &Entry| entry.moment == moment && wanted(entry);
    let url = format!("{}/{}", dav.logs_url(), encode_path(file));
    if let Some(offset) = offset {
        let bytes = dav
            .read_range(&url, offset, offset + RECORD_BYTES - 1)
            .await
            .with_context(|| format!("cannot read {file}"))?;
        // The record is the first one there; a cut at the end of the range only cuts the next.
        let mut parser = EntryParser::new(file);
        let mut first = None;
        parse_lines(&mut parser, &bytes, offset, &mut |entry| {
            first.get_or_insert(entry);
        });
        if let Some(entry) = first
            .or_else(|| parser.finish())
            .filter(|entry| wanted(entry))
        {
            return Ok(Some(entry));
        }
    }

    // In the log folder, from the start: the offset was not where the record is.
    let mut parser = EntryParser::new(file);
    let (mut fetched, mut found) = (0, None);
    let mut keep = |entry: Entry| {
        if found.is_none() && wanted(&entry) {
            found = Some(entry);
        }
    };
    let mut carry = Vec::new();
    loop {
        let bytes = dav
            .read_range(&url, fetched, fetched + SLICE - 1)
            .await
            .with_context(|| format!("cannot read {file}"))?;
        if bytes.is_empty() {
            break;
        }
        let at = fetched - carry.len() as u64;
        fetched += bytes.len() as u64;
        carry.extend_from_slice(&bytes);
        if let Some(end) = carry.iter().rposition(|byte| *byte == b'\n') {
            let complete: Vec<u8> = carry.drain(..=end).collect();
            parse_lines(&mut parser, &complete, at, &mut keep);
        }
    }
    if fetched > 0 {
        let at = fetched - carry.len() as u64;
        parse_lines(&mut parser, &carry, at, &mut keep);
        if let Some(entry) = parser.finish() {
            keep(entry);
        }
        return Ok(found);
    }

    // Not in the log folder any more: moved to log_archive, gzipped or not.
    let Some(day) = file_day(file) else {
        return Ok(None);
    };
    let archived = archive_files(dav, day, &["all".to_string()]).await?;
    let Some(archived) = archived
        .iter()
        .find(|archived| archived.name.strip_suffix(".gz").unwrap_or(&archived.name) == file)
    else {
        return Ok(None);
    };
    read_archived_each(dav, archived, keep).await?;
    Ok(found)
}

/// Lines into entries as they come: each entry is handed over once the next one starts.
pub struct EntryParser {
    file: String,
    label: String,
    current: Option<Entry>,
}

impl EntryParser {
    pub fn new(file: &str) -> EntryParser {
        EntryParser {
            file: file.to_string(),
            label: file.split('-').next().unwrap_or(file).to_string(),
            current: None,
        }
    }

    /// The entry this line closes, when it starts the next one.
    pub fn line(&mut self, line: &str) -> Option<Entry> {
        self.next(line, None)
    }

    /// [`EntryParser::line`], for a line that starts at `offset` of the file.
    pub fn line_at(&mut self, line: &str, offset: u64) -> Option<Entry> {
        self.next(line, Some(offset))
    }

    fn next(&mut self, line: &str, offset: Option<u64>) -> Option<Entry> {
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
                file: self.file.clone(),
                offset,
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
