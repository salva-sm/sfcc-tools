//! The layout does not depend on colour, so a hook's output read in a pipe says the same thing.

use chrono::{DateTime, Local};
use regex::Regex;
use std::io::IsTerminal;
use std::sync::{LazyLock, OnceLock};

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const MAGENTA: &str = "\x1b[35m";
const CYAN: &str = "\x1b[36m";
const NEW_BADGE: &str = "\x1b[1;97;41m";
const BACK_BADGE: &str = "\x1b[1;97;45m";
const SPIKE_BADGE: &str = "\x1b[1;30;43m";
const MESSAGE_CHARS: usize = 160;

static STYLE: OnceLock<Style> = OnceLock::new();

#[derive(Clone, Copy)]
struct Style {
    color: bool,
    problems: bool,
}

pub fn configure(color: &str, problems: bool) {
    let color = match color {
        "always" => true,
        "never" => false,
        _ => std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none(),
    };
    if color {
        enable_virtual_terminal();
    }
    let _ = STYLE.set(Style { color, problems });
}

fn style() -> Style {
    *STYLE.get_or_init(|| Style {
        color: false,
        problems: false,
    })
}

pub fn problems() -> bool {
    style().problems
}

fn paint(tone: &str, text: &str) -> String {
    match style().color && !tone.is_empty() {
        true => format!("{tone}{text}{RESET}"),
        false => text.to_string(),
    }
}

#[derive(Clone, Copy)]
pub enum Tone {
    Ok,
    New,
    Pending,
    Warn,
    Info,
}

/// For a problem matcher, keeps the `log-diff:` prefix its begin and end patterns look for.
pub fn status(tone: Tone, message: &str) {
    if problems() {
        println!("log-diff: {message}");
        return;
    }
    let (mark, color) = match tone {
        Tone::Ok => ("✔", GREEN),
        Tone::New => ("✖", RED),
        Tone::Pending => ("●", YELLOW),
        Tone::Warn => ("▲", YELLOW),
        Tone::Info => ("·", CYAN),
    };
    let message = match tone {
        Tone::New => paint(BOLD, message),
        _ => message.to_string(),
    };
    println!("{} {} {message}", paint(DIM, &stamp()), paint(color, mark));
}

pub fn error(message: &str) {
    match problems() {
        true => eprintln!("log-diff: {message}"),
        false => eprintln!("{} {} {message}", paint(DIM, &stamp()), paint(RED, "ERR")),
    }
}

fn stamp() -> String {
    Local::now().format("%H:%M:%S").to_string()
}

#[derive(Clone, Copy, PartialEq)]
pub enum Badge {
    New,
    Back,
    Spike,
    Pending,
    None,
}

pub struct Card<'a> {
    pub id: &'a str,
    pub label: &'a str,
    pub exception: Option<&'a str>,
    pub location: Option<&'a str>,
    /// The message, then `  at` frames.
    pub example: &'a str,
    pub count: u64,
    pub first_seen: &'a str,
    pub last_seen: Option<&'a str>,
    pub badge: Badge,
    /// Already worded.
    pub deploy: Option<String>,
    pub serious: bool,
}
pub fn card(card: &Card) -> String {
    let (mark, tone) = level(card.label);
    let what = card.exception.unwrap_or(match card.label {
        "fatal" => "Fatal error",
        "customerror" => "Custom error",
        "error" => "Error",
        other => other,
    });
    let head = card.example.lines().next().unwrap_or_default();
    let mut meta: Vec<String> = Vec::new();
    if card.serious {
        meta.push(paint(RED, "500"));
    }
    meta.extend(controller(head).map(|name| paint(DIM, name)));
    meta.push(paint(DIM, card.label));
    meta.push(paint(DIM, &format!("x{}", card.count)));
    let badge = match card.badge {
        Badge::New => format!("  {}", paint(NEW_BADGE, " NEW ")),
        Badge::Back => format!("  {}", paint(BACK_BADGE, " BACK ")),
        Badge::Spike => format!("  {}", paint(SPIKE_BADGE, " SPIKE ")),
        Badge::Pending => format!("  {}", paint(YELLOW, "pending")),
        Badge::None => String::new(),
    };
    let mut lines = vec![format!(
        " {} {}{badge}  {}",
        paint(tone, mark),
        paint(BOLD, what),
        meta.join(&paint(DIM, " · ")),
    )];

    let mut example = card.example.lines();
    lines.push(format!(
        "   {}",
        message(example.next().unwrap_or_default(), card.exception)
    ));

    let function = example
        .find_map(|frame| frame.trim().strip_prefix("at "))
        .and_then(|frame| frame.split_once(" (").map(|(_, function)| function))
        .map(|function| function.trim_end_matches(')'))
        .filter(|function| !function.is_empty() && *function != "anonymous");
    if let Some(location) = card.location {
        let within = function
            .map(|name| format!(" in {name}"))
            .unwrap_or_default();
        lines.push(format!(
            "   {} {location}{}",
            paint(DIM, "↳"),
            paint(DIM, &within)
        ));
    }

    let mut when = Vec::new();
    if !card.first_seen.is_empty() {
        when.push(format!("first {}", moment(card.first_seen)));
    }
    if let Some(last) = card.last_seen.filter(|last| *last != card.first_seen) {
        when.push(format!("last {}", moment(last)));
    }
    if let Some(deploy) = &card.deploy {
        when.push(deploy.clone());
    }
    when.push(card.id.to_string());
    lines.push(format!("   {}", paint(DIM, &when.join(" · "))));
    lines.join("\n")
}

/// Drops the thread and category, a leading `Exception:` already in the title, and a trailing
/// `(file.js#214)` the location line already shows.
pub fn message(head: &str, exception: Option<&str>) -> String {
    static POSITION: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\s*\([^()\s]+#\d+\)\s*$").expect("the pattern is a valid regex")
    });

    let mut message = head.split_once("[] ").map_or(head, |(_, rest)| rest);
    if let Some(exception) = exception
        && let Some(at) = message.rfind(&format!("{exception}: "))
    {
        message = &message[at + exception.len() + 2..];
    }
    let message = POSITION.replace(message, "");
    let mut short: String = message.chars().take(MESSAGE_CHARS).collect();
    if message.chars().count() > MESSAGE_CHARS {
        short.push('…');
    }
    short
}

/// `Cart-AddProduct`, out of the thread
/// `PipelineCallServlet|Sites-Acme-Site|Cart-AddProduct|PipelineCall`.
pub fn controller(head: &str) -> Option<&str> {
    let before = head.split_once("[] ").map_or(head, |(before, _)| before);
    let thread = before.split_whitespace().find(|word| word.contains('|'))?;
    thread.split('|').find(|segment| {
        let Some((name, action)) = segment.split_once('-') else {
            return false;
        };
        name != "Sites"
            && name.starts_with(|c: char| c.is_ascii_uppercase())
            && !action.is_empty()
            && !action.contains('-')
    })
}

fn level(label: &str) -> (&'static str, &'static str) {
    match label {
        "fatal" => ("‼", RED),
        "error" => ("✖", RED),
        "customerror" => ("✖", MAGENTA),
        "warn" | "customwarn" => ("▲", YELLOW),
        _ => ("●", CYAN),
    }
}

fn moment(rfc3339: &str) -> String {
    let Ok(moment) = DateTime::parse_from_rfc3339(rfc3339) else {
        return rfc3339.to_string();
    };
    let local = moment.with_timezone(&Local);
    match local.date_naive() == Local::now().date_naive() {
        true => local.format("%H:%M").to_string(),
        false => local.format("%d/%m %H:%M").to_string(),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(badge: Badge) -> Card<'static> {
        Card {
            id: "4cfc684705f583bc",
            label: "error",
            exception: Some("TypeError"),
            location: Some("app_acme/cartridge/scripts/checkout/CheckoutServices.js:214"),
            example: "ERROR PipelineCallServlet|Sites-Acme-Site|Checkout-Begin|PipelineCall custom.checkout [] \
                      Error while executing script: Wrapped com.x.PipelineExecutionException: TypeError: \
                      Cannot read property \"shipments\" from null (app_acme/cartridge/scripts/checkout/CheckoutServices.js#214)\n  \
                      at app_acme/cartridge/scripts/checkout/CheckoutServices.js:214 (validateBasket)",
            count: 3,
            first_seen: "not a timestamp",
            last_seen: None,
            badge,
            deploy: None,
            serious: true,
        }
    }

    #[test]
    fn a_card_reads_as_what_failed_where_and_when() {
        let lines: Vec<String> = card(&sample(Badge::New))
            .lines()
            .map(str::to_string)
            .collect();

        assert_eq!(
            lines[0],
            " ✖ TypeError   NEW   500 · Checkout-Begin · error · x3"
        );
        assert_eq!(lines[1], "   Cannot read property \"shipments\" from null");
        assert_eq!(
            lines[2],
            "   ↳ app_acme/cartridge/scripts/checkout/CheckoutServices.js:214 in validateBasket"
        );
        assert_eq!(lines[3], "   first not a timestamp · 4cfc684705f583bc");
    }

    #[test]
    fn the_controller_comes_out_of_the_thread() {
        assert_eq!(
            controller(
                "ERROR PipelineCallServlet|Sites-Acme-Site|Cart-AddProduct|PipelineCall custom.cart [] x"
            ),
            Some("Cart-AddProduct")
        );
        assert_eq!(controller("ERROR JobThread|MyJob custom.job [] x"), None);
        assert_eq!(controller("TypeError: boom"), None);
    }

    #[test]
    fn a_message_without_an_exception_keeps_its_words() {
        assert_eq!(
            message(
                "ERROR X|S custom.cart [] Basket <id> has no shipments",
                None
            ),
            "Basket <id> has no shipments"
        );
    }

    #[test]
    fn a_long_message_is_cut() {
        let long = "x".repeat(MESSAGE_CHARS + 10);
        assert_eq!(message(&long, None).chars().count(), MESSAGE_CHARS + 1);
    }
}
