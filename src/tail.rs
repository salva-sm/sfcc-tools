use crate::logging;
use crate::push::Ctx;
use crate::webdav::{DavEntry, encode_path};
use anyhow::Result;
use chrono::Local;
use std::collections::HashMap;
use std::io::IsTerminal;
use std::path::Path;
use std::time::Duration;

pub const DEFAULT_LEVELS: &str = "error,customerror,custom";

const LOOKBACK_BYTES: u64 = 256 * 1024;
const PREFIX_WIDTH: usize = 24;

const RESET: &str = "\x1b[0m";
const DIM: &str = "\x1b[2m";
const RED: &str = "\x1b[31m";
const YELLOW: &str = "\x1b[33m";
const CYAN: &str = "\x1b[36m";
const LINK: &str = "\x1b[1;36m";

pub struct TailOptions {
    pub levels: Vec<String>,
    pub interval: Duration,
    pub lines: usize,
    pub color: bool,
}

pub struct Entry {
    pub label: String,
    pub moment: String,
    pub lines: Vec<String>,
}

pub struct Printer<'a> {
    cartridges: &'a Path,
    color: bool,
    history: usize,
}

pub async fn follow(ctx: &Ctx, options: TailOptions) -> Result<()> {
    let base = ctx.config.logs_url();
    let printer = Printer {
        cartridges: &ctx.config.cartridges_dir,
        color: options.color,
        history: options.lines,
    };
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

        let bootstrap = !announced;
        if bootstrap {
            logging::ok(format!(
                "following {} log file(s) on {}",
                wanted.len(),
                ctx.config.hostname
            ));
            announced = true;
        }

        let mut batch: Vec<Entry> = Vec::new();

        for entry in wanted {
            let url = format!("{base}/{}", encode_path(&entry.name));

            if !offsets.contains_key(&entry.name) {
                if bootstrap {
                    offsets.insert(entry.name.clone(), entry.size);
                    printer.history(ctx, entry).await;
                    continue;
                }
                // Opened after the session started, so it holds the error being waited for.
                offsets.insert(entry.name.clone(), 0);
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
                    batch.extend(parse_entries(&entry.name, &text));
                }
                Err(error) => logging::warn(format!("{}: {error:#}", entry.name)),
            }
        }

        order(&mut batch);
        for entry in &batch {
            printer.entry(entry);
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

pub fn color_enabled(when: &str) -> bool {
    match when {
        "always" => true,
        "never" => false,
        _ => std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none(),
    }
}

impl<'a> Printer<'a> {
    /// A printer with no history to replay, for one-shot reports.
    pub fn plain(cartridges: &'a Path, color: bool) -> Printer<'a> {
        Printer { cartridges, color, history: 0 }
    }
}

impl Printer<'_> {
    async fn history(&self, ctx: &Ctx, file: &DavEntry) {
        if self.history == 0 {
            return;
        }

        let start = file.size.saturating_sub(LOOKBACK_BYTES);
        let url = format!("{}/{}", ctx.config.logs_url(), encode_path(&file.name));
        let Ok(text) = ctx.dav.read_from(&url, start).await else {
            return;
        };

        let mut collected: Vec<&str> = text.lines().collect();
        if start > 0 && !collected.is_empty() {
            collected.remove(0);
        }
        let from = collected.len().saturating_sub(self.history);

        for entry in parse_entries(&file.name, &collected[from..].join("\n")) {
            self.entry(&entry);
        }
    }

    pub fn entry(&self, entry: &Entry) {
        let tone = tone(&entry.label);
        let mut lines = entry.lines.iter();
        let Some(head) = lines.next() else {
            return;
        };

        crate::out!(
            "{} {} {}",
            self.paint(DIM, &format!("[{}]", logging::stamp())),
            self.paint(tone, &format!("{:<12}", entry.label)),
            self.body(head, tone)
        );
        for line in lines {
            crate::out!("{:width$}{}", "", self.body(line, DIM), width = PREFIX_WIDTH);
        }
    }

    fn body(&self, line: &str, tone: &str) -> String {
        let Some((frame, path, number)) = parse_frame(line) else {
            return self.paint(tone, line);
        };

        let local = self.cartridges.join(path.replace('/', std::path::MAIN_SEPARATOR_STR));
        if !local.is_file() {
            return self.paint(tone, line);
        }

        let target = format!("{}:{number}", local.display());
        let (before, after) = line.split_once(frame).expect("the frame comes from the line");
        format!("{}{}{}", self.paint(tone, before), self.paint(LINK, &target), self.paint(tone, after))
    }

    fn paint(&self, tone: &str, text: &str) -> String {
        match self.color && !tone.is_empty() {
            true => format!("{tone}{text}{RESET}"),
            false => text.to_string(),
        }
    }
}

pub fn parse_entries(file: &str, text: &str) -> Vec<Entry> {
    let label = file.split('-').next().unwrap_or(file).to_string();
    let mut entries: Vec<Entry> = Vec::new();

    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        match (moment(line), entries.last_mut()) {
            (None, Some(entry)) => entry.lines.push(line.to_string()),
            (moment, _) => entries.push(Entry {
                label: label.clone(),
                moment: moment.unwrap_or_default(),
                lines: vec![line.to_string()],
            }),
        }
    }
    entries
}

/// Leftovers from an entry of an earlier poll carry no moment and stay in front.
pub fn order(batch: &mut [Entry]) {
    batch.sort_by(|left, right| left.moment.cmp(&right.moment));
}

/// The timestamp every record opens with: `[2026-09-09 07:26:29.103 GMT]`.
fn moment(line: &str) -> Option<String> {
    let inner = line.strip_prefix('[')?.split_once(']')?.0;
    let shape = inner.as_bytes();
    if shape.len() < 19 || shape[4] != b'-' || shape[7] != b'-' || shape[13] != b':' {
        return None;
    }
    Some(inner.to_string())
}

fn tone(label: &str) -> &'static str {
    match label {
        "error" | "customerror" | "fatal" => RED,
        "warn" | "customwarn" | "quota" => YELLOW,
        label if label.starts_with("custom") => CYAN,
        _ => "",
    }
}

pub fn is_wanted(name: &str, levels: &[String], today: &str) -> bool {
    if !name.ends_with(".log") || !name.contains(today) {
        return false;
    }
    levels.iter().any(|level| level == "all" || name.starts_with(level.as_str()))
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

    #[test]
    fn a_stack_trace_belongs_to_the_record_above_it() {
        let text = concat!(
            "[2026-09-09 07:26:29.103 GMT] ERROR PipelineCallServlet custom [] TypeError\n",
            "\tat app_common_brand/cartridge/controllers/Account.js:99 (anonymous)\n",
            "\tat modules/server/route.js:83 (next)\n",
            "[2026-09-09 07:26:30.000 GMT] ERROR PipelineCallServlet custom [] another one\n"
        );

        let entries = parse_entries("error-blade1-20260909.log", text);

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].label, "error");
        assert_eq!(entries[0].lines.len(), 3);
        assert_eq!(entries[1].lines.len(), 1);
    }

    #[test]
    fn lines_arriving_without_a_record_of_their_own_open_one() {
        let entries = parse_entries("error-blade1-20260909.log", "\tat modules/server/route.js:83");

        assert_eq!(entries.len(), 1);
        assert!(entries[0].moment.is_empty());
    }

    #[test]
    fn records_reach_the_screen_in_the_order_the_sandbox_wrote_them() {
        let mut batch = parse_entries("error-blade1-20260909.log", "[2026-09-09 07:26:31.000 GMT] late");
        batch.extend(parse_entries("customerror-blade1-20260909.log", "[2026-09-09 07:26:29.000 GMT] early"));
        batch.extend(parse_entries("error-blade1-20260909.log", "\tat modules/server/route.js:83"));

        order(&mut batch);

        let moments: Vec<&str> = batch.iter().map(|entry| entry.moment.as_str()).collect();
        assert_eq!(moments, ["", "2026-09-09 07:26:29.000 GMT", "2026-09-09 07:26:31.000 GMT"]);
    }

    #[test]
    fn reads_the_moment_only_out_of_a_real_timestamp() {
        assert_eq!(moment("[2026-09-09 07:26:29.103 GMT] ERROR").as_deref(), Some("2026-09-09 07:26:29.103 GMT"));
        assert!(moment("\tat modules/server/route.js:83 (next)").is_none());
        assert!(moment("[main] Quota object.CouponPO").is_none());
    }

    #[test]
    fn plain_text_is_left_untouched_without_colour() {
        let printer = Printer { cartridges: Path::new("/nowhere"), color: false, history: 0 };

        assert_eq!(printer.paint(RED, "boom"), "boom");
        assert_eq!(printer.body("\tat modules/server/route.js:83", RED), "\tat modules/server/route.js:83");
    }

    #[test]
    fn colour_wraps_the_line_in_the_tone_of_its_level() {
        let printer = Printer { cartridges: Path::new("/nowhere"), color: true, history: 0 };

        assert_eq!(printer.paint(RED, "boom"), format!("{RED}boom{RESET}"));
        assert_eq!(printer.paint("", "boom"), "boom");
        assert_eq!(tone("customerror"), RED);
        assert_eq!(tone("customdebug"), CYAN);
        assert_eq!(tone("info"), "");
    }

    #[test]
    fn the_colour_switch_obeys_the_user_before_the_terminal() {
        assert!(color_enabled("always"));
        assert!(!color_enabled("never"));
    }
}
