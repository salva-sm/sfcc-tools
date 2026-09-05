use crate::logging;
use crate::push::Ctx;
use crate::webdav::{DavEntry, encode_path};
use anyhow::Result;
use chrono::Local;
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

pub const DEFAULT_LEVELS: &str = "error,customerror,custom";

const LOOKBACK_BYTES: u64 = 256 * 1024;

pub struct TailOptions {
    pub levels: Vec<String>,
    pub interval: Duration,
    pub lines: usize,
}

pub async fn follow(ctx: &Ctx, options: TailOptions) -> Result<()> {
    let base = ctx.config.logs_url();
    let mut offsets: HashMap<String, u64> = HashMap::new();
    let mut announced = false;

    loop {
        let today = Local::now().format("%Y%m%d").to_string();
        let entries = match ctx.dav.list(&base).await {
            Ok(entries) => entries,
            Err(error) => {
                logging::warn(format!("cannot list the sandbox logs: {error:#}"));
                tokio::time::sleep(options.interval).await;
                continue;
            }
        };

        let wanted: Vec<_> = entries
            .iter()
            .filter(|entry| !entry.is_dir && is_wanted(&entry.name, &options.levels, &today))
            .collect();

        if !announced {
            logging::ok(format!(
                "following {} log file(s) on {}",
                wanted.len(),
                ctx.config.hostname
            ));
            announced = true;
        }

        for entry in wanted {
            let url = format!("{base}/{}", encode_path(&entry.name));

            if !offsets.contains_key(&entry.name) {
                offsets.insert(entry.name.clone(), entry.size);
                if options.lines > 0 {
                    print_history(ctx, entry, options.lines).await;
                }
                continue;
            }

            let offset = offsets.get_mut(&entry.name).expect("just inserted above");
            if entry.size < *offset {
                *offset = 0;
            }
            if entry.size <= *offset {
                continue;
            }

            match ctx.dav.read_from(&url, *offset).await {
                Ok(text) => {
                    *offset += text.len() as u64;
                    print_lines(&entry.name, &text, &ctx.config.cartridges_dir);
                }
                Err(error) => logging::warn(format!("{}: {error:#}", entry.name)),
            }
        }

        tokio::time::sleep(options.interval).await;
    }
}

pub fn parse_levels(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|level| level.trim().to_lowercase())
        .filter(|level| !level.is_empty())
        .collect()
}

async fn print_history(ctx: &Ctx, entry: &DavEntry, lines: usize) {
    let start = entry.size.saturating_sub(LOOKBACK_BYTES);
    let url = format!("{}/{}", ctx.config.logs_url(), encode_path(&entry.name));

    let Ok(text) = ctx.dav.read_from(&url, start).await else {
        return;
    };

    let mut collected: Vec<&str> = text.lines().collect();
    if start > 0 && !collected.is_empty() {
        collected.remove(0);
    }
    let from = collected.len().saturating_sub(lines);
    print_lines(&entry.name, &collected[from..].join("\n"), &ctx.config.cartridges_dir);
}

fn is_wanted(name: &str, levels: &[String], today: &str) -> bool {
    if !name.ends_with(".log") || !name.contains(today) {
        return false;
    }
    levels.iter().any(|level| level == "all" || name.starts_with(level.as_str()))
}

fn print_lines(file: &str, text: &str, cartridges_dir: &Path) {
    let label = file.split('-').next().unwrap_or(file);
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        crate::out!("[{}] {label:<12} {}", logging::stamp(), localize(line, cartridges_dir));
    }
}

fn localize(line: &str, cartridges_dir: &Path) -> String {
    let Some((frame, path, number)) = parse_frame(line) else {
        return line.to_string();
    };

    let local = cartridges_dir.join(path.replace('/', std::path::MAIN_SEPARATOR_STR));
    if !local.is_file() {
        return line.to_string();
    }
    line.replacen(frame, &format!("{}:{number}", local.display()), 1)
}

fn parse_frame(line: &str) -> Option<(&str, &str, &str)> {
    let frame = line.trim_start().strip_prefix("at ")?.split_whitespace().next()?;
    let (path, number) = frame.rsplit_once(':')?;
    if path.is_empty() || number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    if !path.contains('/') || path.contains('[') {
        return None;
    }
    Some((frame, path, number))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_only_todays_files_of_the_wanted_levels() {
        let levels = parse_levels(DEFAULT_LEVELS);
        assert!(is_wanted("error-blade1-1-appserver-20260905.log", &levels, "20260905"));
        assert!(is_wanted("customerror-blade1-20260905.log", &levels, "20260905"));
        assert!(!is_wanted("warn-blade1-20260905.log", &levels, "20260905"));
        assert!(!is_wanted("error-blade1-20260904.log", &levels, "20260905"));
        assert!(!is_wanted("error-blade1-20260905.txt", &levels, "20260905"));
    }

    #[test]
    fn the_all_level_keeps_every_log_of_the_day() {
        let levels = parse_levels("all");
        assert!(is_wanted("warn-blade1-20260905.log", &levels, "20260905"));
        assert!(!is_wanted("warn-blade1-20260904.log", &levels, "20260905"));
    }

    #[test]
    fn reads_the_file_and_line_out_of_a_stack_frame() {
        let frame = "\tat app_common_brand/cartridge/controllers/Account.js:99 (anonymous)";
        assert_eq!(
            parse_frame(frame),
            Some((
                "app_common_brand/cartridge/controllers/Account.js:99",
                "app_common_brand/cartridge/controllers/Account.js",
                "99"
            ))
        );
        assert_eq!(parse_frame("\tat modules/server/route.js:83 (next)").map(|f| f.2), Some("83"));
    }

    #[test]
    fn leaves_alone_anything_that_is_not_a_file_frame() {
        assert!(parse_frame("\tat [Template:common/layout/page:${pdict.category.ID}]:1").is_none());
        assert!(parse_frame("TypeError: Cannot read property \"ID\" from null").is_none());
        assert!(parse_frame("\tat something-without-a-line.js (main)").is_none());
        assert!(parse_frame("\tat bare.js:12 (main)").is_none());
    }
}
