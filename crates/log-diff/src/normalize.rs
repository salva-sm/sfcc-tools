//! Line numbers stay out of the hash: an unrelated edit higher up a file shifts them.
//! Scrubbing keeps customer data out of the ledger; the request dump after the stack is dropped.

use regex::{Captures, Regex};
use sfcc_core::logs::Entry;
use std::sync::LazyLock;
use xxhash_rust::xxh3::xxh3_64;

/// Raise it with any change that gives a failure a different id, so old ledgers relearn quietly
/// instead of reporting everything as new.
pub const SIGNATURES: u32 = 1;
/// Below this the stack is the framework's, the same for every failure.
const FRAMES: usize = 8;
const EXAMPLE_FRAMES: usize = 3;
const EXAMPLE_CHARS: usize = 400;

#[derive(Debug, Clone, PartialEq)]
pub struct Signature {
    /// Stable across runs, machines and versions.
    pub id: String,
    /// The level, as the file name spells it.
    pub label: String,
    /// The innermost one named in the message.
    pub exception_class: Option<String>,
    /// `cartridge/path/file.js:214`.
    pub location: Option<String>,
    /// Scrubbed, line numbers included.
    pub message: String,
    /// Scrubbed, line numbers included.
    pub frames: Vec<String>,
}

impl Signature {
    pub fn example(&self) -> String {
        let mut example: String = self.message.chars().take(EXAMPLE_CHARS).collect();
        for frame in self.frames.iter().take(EXAMPLE_FRAMES) {
            example.push_str("\n  at ");
            example.push_str(frame);
        }
        example
    }
}

macro_rules! regex {
    ($name:ident, $pattern:expr) => {
        static $name: LazyLock<Regex> =
            LazyLock::new(|| Regex::new($pattern).expect("the pattern is a valid regex"));
    };
}

// `[moment] LEVEL thread|with|segments category []`.
regex!(HEADER, r"^\[[^\]]*\]\s*");
regex!(THREAD, r"^(\w+)\s+(\S*\|\S*)\s*");
// Scrubbers, applied in this order.
regex!(
    TIMESTAMP,
    r"\d{4}-\d{2}-\d{2}(?:[ T]\d{2}:\d{2}(?::\d{2}(?:[.,]\d+)?)?(?:\s?(?:GMT|UTC|Z|[+-]\d{2}:?\d{2}))?)?"
);
regex!(
    UUID,
    r"\b[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}\b"
);
regex!(EMAIL, r"[\w.+-]+@[\w-]+(?:\.[\w-]+)+");
regex!(URL, r#"https?://[^\s"'<>()]+"#);
regex!(IP, r"\b\d{1,3}(?:\.\d{1,3}){3}\b");
regex!(
    SECRET,
    r#"(?i)\b(dwsid|dwsecuretoken\w*|dwanonuid\w*|token|access_token|sessionid|password|passwd|pwd|authorization|cookie|api[_-]?key|secret|card(?:number)?|cvv|iban)(\s*[=:]\s*)"?[^\s"',;&)]+"?"#
);
regex!(BEARER, r"(?i)\b(bearer|basic)\s+[A-Za-z0-9._~+/=\-]{8,}");
regex!(DECIMAL, r"\b\d+[.,]\d+\b");
regex!(WORD, r"[A-Za-z0-9_\-]+");
// A script position, in a frame (`file.js:214`) or inside a message (`file.js#214`).
regex!(POSITION, r"(\.(?:js|ds|isml))[:#](\d+)");
// `[Template:account/editProfileForm:${pdict.x}]:1`: an expression inside an ISML template.
regex!(TEMPLATE_FRAME, r"^\[Template:/?([^:\]]+)");
regex!(
    MESSAGE_LOCATION,
    r"([A-Za-z0-9_\-]+/cartridge/[^\s()#:]+\.(?:js|ds|isml))#(\d+)"
);
regex!(
    EXCEPTION,
    r"\b(?:[a-z_][\w]*\.)*([A-Z]\w*(?:Error|Exception))\b"
);

pub fn signature(entry: &Entry) -> Signature {
    let mut lines = entry.lines.iter();
    let head = lines.next().map(String::as_str).unwrap_or_default();
    let message = scrub(&strip_header(head));

    let frames: Vec<String> = lines
        .filter_map(|line| line.trim_start().strip_prefix("at "))
        .take(FRAMES)
        .map(|frame| scrub_frame(frame.trim()))
        .collect();

    let mut hashed = format!("{}\n{}", entry.label, without_positions(&message));
    for frame in &frames {
        hashed.push('\n');
        hashed.push_str(&without_positions(frame));
    }

    Signature {
        id: format!("{:016x}", xxh3_64(hashed.as_bytes())),
        label: entry.label.clone(),
        exception_class: EXCEPTION
            .captures_iter(&message)
            .last()
            .map(|found| found[1].to_string()),
        location: location(&frames, &message),
        message,
        frames,
    }
}

/// Drops the moment, thread number and session: of
/// `PipelineCallServlet|157318437|Sites-Site|Cart-Show|PipelineCall|t1ZZ-bCb`, the servlet, site
/// and controller stay.
fn strip_header(head: &str) -> String {
    let rest = HEADER.replace(head, "");
    let Some(found) = THREAD.captures(&rest) else {
        return rest.into_owned();
    };

    let segments: Vec<&str> = found[2].split('|').collect();
    let keep = match segments.len() {
        count if count >= 5 => &segments[..count - 1],
        _ => &segments[..],
    };
    let thread: Vec<&str> = keep
        .iter()
        .copied()
        .filter(|segment| !segment.is_empty() && !segment.bytes().all(|b| b.is_ascii_digit()))
        .collect();

    format!(
        "{} {} {}",
        &found[1],
        thread.join("|"),
        &rest[found.get(0).map_or(0, |whole| whole.end())..]
    )
}

pub fn scrub(text: &str) -> String {
    let text = TIMESTAMP.replace_all(text, "<time>");
    let text = UUID.replace_all(&text, "<uuid>");
    // Before the email rule, which would cut a query string in two.
    let text = URL.replace_all(&text, |found: &Captures| scrub_url(&found[0]));
    let text = EMAIL.replace_all(&text, "<email>");
    let text = IP.replace_all(&text, "<ip>");
    let text = SECRET.replace_all(&text, "$1$2<redacted>");
    let text = BEARER.replace_all(&text, "$1 <redacted>");
    let text = DECIMAL.replace_all(&text, "<n>");
    let text = scrub_words(&text);
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn scrub_url(url: &str) -> String {
    let bare = url.split(['?', '#']).next().unwrap_or(url);
    let Some((scheme, rest)) = bare.split_once("://") else {
        return bare.to_string();
    };
    let mut parts = rest.split('/');
    let host = parts.next().unwrap_or_default();
    let mut scrubbed = format!("{scheme}://{host}");
    for segment in parts {
        scrubbed.push('/');
        scrubbed.push_str(&scrub_words(segment));
    }
    scrubbed
}

/// Short numbers stay (`404`, `line 3` mean something); paths stay, as a cartridge name may
/// have a digit.
fn scrub_words(text: &str) -> String {
    let mut scrubbed = String::with_capacity(text.len());
    for (index, word) in text.split(' ').enumerate() {
        if index > 0 {
            scrubbed.push(' ');
        }
        if word.contains('/') && !word.starts_with("http") {
            scrubbed.push_str(word);
            continue;
        }
        let replaced = WORD.replace_all(word, |found: &Captures| {
            let token = &found[0];
            let digits = token.bytes().filter(u8::is_ascii_digit).count();
            let letters = token.bytes().filter(u8::is_ascii_alphabetic).count();
            if letters == 0 && digits >= 4 {
                "<n>".to_string()
            } else if letters > 0 && digits > 0 && token.len() >= 6 {
                "<id>".to_string()
            } else {
                token.to_string()
            }
        });
        scrubbed.push_str(&replaced);
    }
    scrubbed
}

/// Only a template frame is volatile: it can quote an expression's value.
fn scrub_frame(frame: &str) -> String {
    match frame.starts_with('[') {
        true => scrub(frame),
        false => frame.split_whitespace().collect::<Vec<_>>().join(" "),
    }
}

fn without_positions(text: &str) -> String {
    POSITION.replace_all(text, "$1").into_owned()
}

fn location(frames: &[String], message: &str) -> Option<String> {
    let from_frames = frames.iter().find_map(|frame| {
        // Its line is the expression's, not the file's; the frames under it are all render.js.
        if let Some(template) = TEMPLATE_FRAME.captures(frame) {
            return Some(format!("{}.isml", &template[1]));
        }
        let position = frame.split_whitespace().next()?;
        let (path, line) = position.rsplit_once(':')?;
        let is_script = path.contains('/') && POSITION.is_match(position);
        (is_script && line.bytes().all(|b| b.is_ascii_digit())).then(|| format!("{path}:{line}"))
    });
    from_frames.or_else(|| {
        MESSAGE_LOCATION
            .captures(message)
            .map(|found| format!("{}:{}", &found[1], &found[2]))
    })
}

#[cfg(test)]
#[path = "normalize_tests.rs"]
mod tests;
