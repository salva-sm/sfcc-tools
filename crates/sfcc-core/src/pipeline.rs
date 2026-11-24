//! The platform stamps every storefront record with a fixed request head
//! (`servlet|seq|Sites-<site>-Site|controller|callType|reqId`) and a logger
//! before the part that matters. `clean` pulls out the level, the controller
//! and the message, the way the dashboard's log viewer does; a line that is not
//! such a record returns `None` for the caller to keep whole.

/// A storefront record reduced to what is worth showing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub level: Option<String>,
    pub controller: Option<String>,
    pub message: String,
}

/// The record behind a log line, or `None` when it is not a logged record.
pub fn clean(body: &str) -> Option<Record> {
    job(body).or_else(|| head(body))
}

/// `[Thread|seq|jobId|jobName] message`: the job stands in for the controller.
fn job(body: &str) -> Option<Record> {
    let rest = body.strip_prefix('[')?;
    let close = rest.find(']')?;
    let [thread, seq, job_id, name] = rest[..close].split('|').collect::<Vec<_>>()[..] else {
        return None;
    };
    if !is_thread(thread) || !is_digits(seq) || job_id.is_empty() || name.is_empty() {
        return None;
    }
    Some(Record {
        level: None,
        controller: Some(name.to_string()),
        message: rest[close + 1..].trim().to_string(),
    })
}

/// A logged record: its level, the controller of the request that logged it (if
/// any — thread and filter logs carry none), and the message after the logger.
fn head(body: &str) -> Option<Record> {
    let logger = words(body).find(|(_, word)| is_logger(word))?.0;
    let before = &body[..logger];
    Some(Record {
        level: before.split_whitespace().find(|word| is_level(word)).map(str::to_uppercase),
        controller: before
            .split_whitespace()
            .find(|word| word.contains('|'))
            .and_then(|token| token.split('|').find(|segment| is_controller(segment)))
            .map(str::to_string),
        message: message_of(&body[logger..]),
    })
}

/// What a storefront `<logger> [<mdc>] <message>` or system `… - <message>`
/// line is really saying, past its logger and request context.
fn message_of(after: &str) -> String {
    let (logger, rest) = after.split_once(char::is_whitespace).unwrap_or((after, ""));
    // System logs end on a ` - ` separator; custom/dw carry a bracketed context.
    if logger == "system" || logger.starts_with("system.") {
        return after_dash(rest);
    }
    if is_logger(logger) {
        let rest = rest.trim_start();
        return rest
            .strip_prefix('[')
            .and_then(|mdc| mdc.find(']').map(|close| &mdc[close + 1..]))
            .unwrap_or(rest)
            .trim()
            .to_string();
    }
    after_dash(after)
}

fn after_dash(text: &str) -> String {
    match text.split_once(" - ") {
        Some((_, message)) => message.trim().to_string(),
        None => text.trim().to_string(),
    }
}

/// Each whitespace-delimited word with its byte offset in `body`.
fn words(body: &str) -> impl Iterator<Item = (usize, &str)> {
    body.split_whitespace()
        .map(|word| (word.as_ptr() as usize - body.as_ptr() as usize, word))
}

fn is_level(word: &str) -> bool {
    matches!(
        word.to_uppercase().as_str(),
        "DEBUG" | "INFO" | "WARN" | "WARNING" | "ERROR" | "FATAL" | "TRACE" | "SEVERE"
    )
}

/// The logger after the head: `custom`, `dw…` or `system`, bare or dotted.
fn is_logger(word: &str) -> bool {
    let dotted = |root: &str| word == root || word.starts_with(&format!("{root}."));
    dotted("custom")
        || dotted("system")
        || (word.starts_with("dw") && word.as_bytes().get(2).is_some_and(u8::is_ascii_lowercase))
}

/// A `Controller-Action` segment: not the site, a capitalised name, a one-word action.
fn is_controller(segment: &str) -> bool {
    let Some((name, action)) = segment.split_once('-') else {
        return false;
    };
    name != "Sites"
        && name.starts_with(|c: char| c.is_ascii_uppercase())
        && !action.is_empty()
        && !action.contains('-')
}

fn is_thread(word: &str) -> bool {
    word.starts_with(|c: char| c.is_ascii_alphabetic())
        && word.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn is_digits(word: &str) -> bool {
    !word.is_empty() && word.bytes().all(|byte| byte.is_ascii_digit())
}

#[cfg(test)]
#[path = "pipeline_tests.rs"]
mod tests;
