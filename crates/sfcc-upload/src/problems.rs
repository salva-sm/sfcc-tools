//! `watch --problems`: each batch as lines a VS Code background problem matcher reads,
//! so a failed upload shows in Problems and the status bar instead of only the terminal.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

static ENABLED: AtomicBool = AtomicBool::new(false);
/// More than this and Problems becomes a list of the whole checkout.
const MAX_FILES: usize = 20;

pub fn enable() {
    ENABLED.store(true, Ordering::Relaxed);
}

fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Opens a cycle: the matcher clears what the last one reported.
pub fn begin(what: &str) {
    if enabled() {
        println!("sfcc-upload: {what}");
    }
}

pub fn synced() {
    if enabled() {
        println!("sfcc-upload: synced");
    }
}

/// One problem per queued file, or on dw.json when none is a file.
pub fn failed<'a>(queued: impl IntoIterator<Item = &'a PathBuf>, dw_json: &Path, reason: &str) {
    if enabled() {
        for line in failure_lines(queued, dw_json, reason) {
            println!("{line}");
        }
    }
}

fn failure_lines<'a>(
    queued: impl IntoIterator<Item = &'a PathBuf>,
    dw_json: &Path,
    reason: &str,
) -> Vec<String> {
    let mut lines: Vec<String> = queued
        .into_iter()
        .filter(|path| path.is_file())
        .take(MAX_FILES)
        .map(|file| format!("{}:1: error: not on the sandbox - {reason}", file.display()))
        .collect();
    if lines.is_empty() {
        lines.push(format!("{}:1: error: {reason}", dw_json.display()));
    }
    lines.push("sfcc-upload: failed".to_string());
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failure_lands_on_the_files_that_did_not_arrive_and_ends_the_cycle() {
        let dir = std::env::temp_dir().join(format!("sfcc-upload-problems-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let saved = dir.join("a.js");
        std::fs::write(&saved, "x").unwrap();
        let gone = dir.join("deleted.js");

        let lines = failure_lines([&saved, &gone], Path::new("/p/dw.json"), "HTTP 503");
        assert_eq!(
            lines,
            vec![
                format!(
                    "{}:1: error: not on the sandbox - HTTP 503",
                    saved.display()
                ),
                "sfcc-upload: failed".to_string(),
            ]
        );

        let lines = failure_lines([&gone], Path::new("/p/dw.json"), "HTTP 503");
        assert_eq!(lines[0], "/p/dw.json:1: error: HTTP 503");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
