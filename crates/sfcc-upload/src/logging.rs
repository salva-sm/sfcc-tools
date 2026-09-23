//! Terminal output: one timestamped line per event, grouped file lists, colour
//! when a terminal is listening.
//!
//! Colour is decided once per process and read from everywhere, so the detached
//! watcher - whose stdout is a log file - writes plain text without any caller
//! having to know about it.

use chrono::Local;
use std::io::IsTerminal;
use std::sync::OnceLock;

pub const RESET: &str = "\x1b[0m";
pub const DIM: &str = "\x1b[2m";
pub const RED: &str = "\x1b[31m";
pub const GREEN: &str = "\x1b[32m";
pub const YELLOW: &str = "\x1b[33m";
pub const CYAN: &str = "\x1b[36m";
pub const LINK: &str = "\x1b[1;36m";

/// Width of `[HH:MM:SS] `, where continuation lines start.
const STAMP_WIDTH: usize = 11;
/// Indent of the folders under a cartridge.
const FOLDER_INDENT: usize = STAMP_WIDTH + 2;
/// Longest folder still worth aligning the file names against.
const FOLDER_COLUMN: usize = 46;
/// Room for the file names once the folder column is taken.
const NAMES_WIDTH: usize = 72;
/// Folders listed before the rest collapses into a count.
const MAX_FOLDERS: usize = 15;
/// File names listed per folder before the rest collapses into a count.
const MAX_NAMES: usize = 10;

static COLOR: OnceLock<bool> = OnceLock::new();

/// What a batch of paths did, which decides the marker and the tone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Uploaded,
    Deleted,
}

impl Change {
    fn marker(self) -> &'static str {
        match self {
            Change::Uploaded => "->",
            Change::Deleted => "x",
        }
    }

    fn tone(self) -> &'static str {
        match self {
            Change::Uploaded => GREEN,
            Change::Deleted => YELLOW,
        }
    }

    fn verb(self) -> &'static str {
        match self {
            Change::Uploaded => "uploaded",
            Change::Deleted => "deleted",
        }
    }

    /// A deletion can take a whole folder, so its paths are not all files.
    fn noun(self) -> &'static str {
        match self {
            Change::Uploaded => "file(s)",
            Change::Deleted => "path(s)",
        }
    }
}

/// `always`, `never`, or `auto`: a terminal on stdout and no `NO_COLOR`.
pub fn configure_color(when: &str) {
    let enabled = wants_color(when);
    if enabled {
        enable_virtual_terminal();
    }
    let _ = COLOR.set(enabled);
}

pub fn color_enabled() -> bool {
    *COLOR.get_or_init(|| wants_color("auto"))
}

/// Pin the output to plain text, so a test of the layout reads the same whether
/// it runs in a terminal or in a pipe.
#[cfg(test)]
pub fn no_color_in_tests() {
    let _ = COLOR.set(false);
}

pub fn paint(tone: &str, text: &str) -> String {
    match color_enabled() {
        true => tint(tone, text),
        false => text.to_string(),
    }
}

fn wants_color(when: &str) -> bool {
    match when {
        "always" => true,
        "never" => false,
        _ => std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none(),
    }
}

fn tint(tone: &str, text: &str) -> String {
    match tone.is_empty() {
        true => text.to_string(),
        false => format!("{tone}{text}{RESET}"),
    }
}

pub fn stamp() -> String {
    Local::now().format("%H:%M:%S").to_string()
}

pub fn info(message: impl AsRef<str>) {
    line("", "", message.as_ref());
}

pub fn ok(message: impl AsRef<str>) {
    line("OK", GREEN, message.as_ref());
}

pub fn warn(message: impl AsRef<str>) {
    line("!", YELLOW, message.as_ref());
}

pub fn error(message: impl AsRef<str>) {
    crate::errout!(
        "{} {} {}",
        prefix(),
        paint(RED, &format!("{:<3}", "ERR")),
        message.as_ref()
    );
}

/// A batch of uploads or deletions, grouped by cartridge and folder.
pub fn changes(change: Change, paths: &[String]) {
    print(change, paths, true);
}

/// The same grouping without the headline, for a caller that already said what
/// the list is - `push --dry-run`, which announces the counts and the size.
pub fn listing(change: Change, paths: &[String]) {
    print(change, paths, false);
}

fn print(change: Change, paths: &[String], headline: bool) {
    let mut sorted = paths.to_vec();
    sorted.sort();
    for rendered in render(change, &sorted, headline) {
        crate::out!("{rendered}");
    }
}

fn line(marker: &str, tone: &str, message: &str) {
    crate::out!(
        "{} {} {message}",
        prefix(),
        paint(tone, &format!("{marker:<3}"))
    );
}

fn prefix() -> String {
    paint(DIM, &format!("[{}]", stamp()))
}

/// The whole block as lines, so the layout can be tested without a terminal.
fn render(change: Change, paths: &[String], headline: bool) -> Vec<String> {
    if paths.is_empty() {
        return Vec::new();
    }

    let tone = change.tone();
    let marker = paint(tone, &format!("{:<3}", change.marker()));

    if headline && let [only] = paths {
        let (folder, name) = split_last(only);
        let painted = match only.contains('/') {
            false => paint(tone, name),
            true => format!("{}{}", paint(DIM, &format!("{folder}/")), paint(tone, name)),
        };
        return vec![format!(
            "{} {marker} {painted} {}",
            prefix(),
            paint(DIM, change.verb())
        )];
    }

    let bundles = bundle(paths);
    let mut lines: Vec<String> = Vec::new();
    if headline {
        lines.push(format!(
            "{} {marker} {} {} {} in {} cartridge(s)",
            prefix(),
            paths.len(),
            change.noun(),
            change.verb(),
            bundles.len()
        ));
    }

    let mut left = MAX_FOLDERS;
    let mut hidden = 0;

    for bundle in &bundles {
        if left == 0 {
            hidden += bundle
                .folders
                .iter()
                .map(|folder| folder.names.len())
                .sum::<usize>();
            continue;
        }
        lines.push(format!(
            "{:STAMP_WIDTH$}{}",
            "",
            paint(CYAN, &bundle.cartridge)
        ));

        let width = bundle.column();
        for folder in bundle.folders.iter().take(left) {
            lines.extend(folder.lines(width, tone));
        }
        if bundle.folders.len() > left {
            hidden += bundle.folders[left..]
                .iter()
                .map(|folder| folder.names.len())
                .sum::<usize>();
        }
        left = left.saturating_sub(bundle.folders.len());
    }

    if hidden > 0 {
        lines.push(paint(
            DIM,
            &format!(
                "{:FOLDER_INDENT$}... and {hidden} more {}",
                "",
                change.noun()
            ),
        ));
    }
    lines
}

struct Folder {
    path: String,
    names: Vec<String>,
}

impl Folder {
    /// `templates/default/checkout   billing.isml  summary.isml`, wrapped when
    /// the names do not fit and stacked when the folder is too long to align.
    fn lines(&self, column: usize, tone: &str) -> Vec<String> {
        let shown: Vec<&str> = self
            .names
            .iter()
            .take(MAX_NAMES)
            .map(String::as_str)
            .collect();
        let mut names: Vec<String> = wrap(&shown, NAMES_WIDTH)
            .iter()
            .map(|row| paint(tone, row))
            .collect();
        if self.names.len() > MAX_NAMES {
            let extra = self.names.len() - MAX_NAMES;
            names.push(paint(DIM, &format!("+{extra} more")));
        }

        let folder = paint(DIM, &self.path);
        let stacked = self.path.chars().count() > column;
        let mut lines = Vec::with_capacity(names.len() + usize::from(stacked));

        if stacked {
            lines.push(format!("{:FOLDER_INDENT$}{folder}", ""));
        }
        let padding = column.saturating_sub(self.path.chars().count());
        for (index, row) in names.iter().enumerate() {
            let head = match (stacked, index) {
                (false, 0) => format!("{:FOLDER_INDENT$}{folder}{:padding$}", "", ""),
                _ => format!("{:width$}", "", width = FOLDER_INDENT + column),
            };
            lines.push(format!("{head}  {row}"));
        }
        lines
    }
}

struct Bundle {
    cartridge: String,
    folders: Vec<Folder>,
}

impl Bundle {
    fn column(&self) -> usize {
        self.folders
            .iter()
            .map(|folder| folder.path.chars().count())
            .filter(|length| *length <= FOLDER_COLUMN)
            .max()
            .unwrap_or(0)
    }
}

/// Paths to `cartridge -> folder inside it -> file names`, in the order given.
fn bundle(paths: &[String]) -> Vec<Bundle> {
    let mut bundles: Vec<Bundle> = Vec::new();

    for path in paths {
        let (head, rest) = match path.split_once('/') {
            Some((cartridge, rest)) => (cartridge.to_string(), rest),
            None => (path.clone(), ""),
        };

        let bundle = match bundles.iter_mut().find(|bundle| bundle.cartridge == head) {
            Some(existing) => existing,
            None => {
                bundles.push(Bundle {
                    cartridge: head,
                    folders: Vec::new(),
                });
                bundles.last_mut().expect("just pushed")
            }
        };
        // A whole cartridge going away needs nothing under its own name.
        if rest.is_empty() {
            continue;
        }

        let (folder, name) = split_last(rest);
        match bundle.folders.iter_mut().find(|entry| entry.path == folder) {
            Some(existing) => existing.names.push(name.to_string()),
            None => bundle.folders.push(Folder {
                path: folder.to_string(),
                names: vec![name.to_string()],
            }),
        }
    }
    bundles
}

/// `a/b/c.js` -> (`a/b`, `c.js`). A file sitting at the root of a cartridge
/// lands in `.`.
fn split_last(path: &str) -> (&str, &str) {
    match path.rsplit_once('/') {
        Some((folder, name)) => (folder, name),
        None => (".", path),
    }
}

fn wrap(names: &[&str], width: usize) -> Vec<String> {
    let mut rows: Vec<String> = Vec::new();
    let mut current = String::new();

    for name in names {
        if !current.is_empty() && current.chars().count() + name.chars().count() + 2 > width {
            rows.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push_str("  ");
        }
        current.push_str(name);
    }
    if !current.is_empty() {
        rows.push(current);
    }
    rows
}

/// Older consoles print the escapes literally unless this is switched on.
#[cfg(windows)]
fn enable_virtual_terminal() {
    use std::ffi::c_void;

    const STDOUT_HANDLE: u32 = 0xFFFF_FFF5;
    const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetStdHandle(id: u32) -> *mut c_void;
        fn GetConsoleMode(console: *mut c_void, mode: *mut u32) -> i32;
        fn SetConsoleMode(console: *mut c_void, mode: u32) -> i32;
    }

    unsafe {
        let console = GetStdHandle(STDOUT_HANDLE);
        if console.is_null() || console as isize == -1 {
            return;
        }
        let mut mode: u32 = 0;
        if GetConsoleMode(console, &mut mode) != 0 {
            SetConsoleMode(console, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
        }
    }
}

#[cfg(not(windows))]
fn enable_virtual_terminal() {}

#[macro_export]
macro_rules! out {
    ($($arg:tt)*) => {{
        use std::io::Write;
        let _ = writeln!(std::io::stdout(), $($arg)*);
    }};
}

#[macro_export]
macro_rules! outp {
    ($($arg:tt)*) => {{
        use std::io::Write;
        let _ = write!(std::io::stdout(), $($arg)*);
    }};
}

#[macro_export]
macro_rules! errout {
    ($($arg:tt)*) => {{
        use std::io::Write;
        let _ = writeln!(std::io::stderr(), $($arg)*);
    }};
}

#[cfg(test)]
#[path = "logging_tests.rs"]
mod tests;
