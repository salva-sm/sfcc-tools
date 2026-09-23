use crate::logging::{self, CYAN, DIM, LINK, RED, YELLOW};
use crate::push::Ctx;
use crate::webdav::{DavEntry, encode_path};
use anyhow::Result;
pub use sfcc_core::logs::{DEFAULT_LEVELS, Entry, parse_levels};
use sfcc_core::logs::{is_wanted, order, parse_entries, today};
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

const LOOKBACK_BYTES: u64 = 256 * 1024;
const PREFIX_WIDTH: usize = 24;

pub struct TailOptions {
    pub levels: Vec<String>,
    pub interval: Duration,
    pub lines: usize,
}

pub struct Printer<'a> {
    cartridges: &'a Path,
    history: usize,
}

pub async fn follow(ctx: &Ctx, options: TailOptions) -> Result<()> {
    let base = ctx.config.logs_url();
    let printer = Printer {
        cartridges: &ctx.config.cartridges_dir,
        history: options.lines,
    };
    let mut offsets: HashMap<String, u64> = HashMap::new();
    let mut announced = false;

    loop {
        let day = today();
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
            .filter(|entry| !entry.is_dir && is_wanted(&entry.name, &options.levels, &day))
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

impl<'a> Printer<'a> {
    /// A printer with no history to replay, for one-shot reports.
    pub fn plain(cartridges: &'a Path) -> Printer<'a> {
        Printer {
            cartridges,
            history: 0,
        }
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
            logging::paint(DIM, &format!("[{}]", logging::stamp())),
            logging::paint(tone, &format!("{:<12}", entry.label)),
            self.body(head, tone)
        );
        for line in lines {
            crate::out!(
                "{:width$}{}",
                "",
                self.body(line, DIM),
                width = PREFIX_WIDTH
            );
        }
    }

    fn body(&self, line: &str, tone: &str) -> String {
        let Some((frame, path, number)) = parse_frame(line) else {
            return logging::paint(tone, line);
        };

        let local = self
            .cartridges
            .join(path.replace('/', std::path::MAIN_SEPARATOR_STR));
        if !local.is_file() {
            return logging::paint(tone, line);
        }

        let target = format!("{}:{number}", local.display());
        let (before, after) = line
            .split_once(frame)
            .expect("the frame comes from the line");
        format!(
            "{}{}{}",
            logging::paint(tone, before),
            logging::paint(LINK, &target),
            logging::paint(tone, after)
        )
    }
}

fn tone(label: &str) -> &'static str {
    match label {
        "error" | "customerror" | "fatal" => RED,
        "warn" | "customwarn" | "quota" => YELLOW,
        label if label.starts_with("custom") => CYAN,
        _ => "",
    }
}

fn parse_frame(line: &str) -> Option<(&str, &str, &str)> {
    let frame = line
        .trim_start()
        .strip_prefix("at ")?
        .split_whitespace()
        .next()?;
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
        assert_eq!(
            parse_frame("\tat modules/server/route.js:83 (next)").map(|f| f.2),
            Some("83")
        );
    }

    #[test]
    fn leaves_alone_anything_that_is_not_a_file_frame() {
        assert!(parse_frame("\tat [Template:common/layout/page:${pdict.category.ID}]:1").is_none());
        assert!(parse_frame("TypeError: Cannot read property \"ID\" from null").is_none());
        assert!(parse_frame("\tat something-without-a-line.js (main)").is_none());
        assert!(parse_frame("\tat bare.js:12 (main)").is_none());
    }

    #[test]
    fn a_frame_pointing_nowhere_local_is_left_untouched() {
        logging::no_color_in_tests();
        let printer = Printer {
            cartridges: Path::new("/nowhere"),
            history: 0,
        };

        assert_eq!(
            printer.body("\tat modules/server/route.js:83", RED),
            "\tat modules/server/route.js:83"
        );
    }

    #[test]
    fn each_level_carries_its_own_tone() {
        assert_eq!(tone("customerror"), RED);
        assert_eq!(tone("customdebug"), CYAN);
        assert_eq!(tone("info"), "");
    }
}
