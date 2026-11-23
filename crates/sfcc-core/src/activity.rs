//! What an uploader activity line means, defined once so the uploader's colour
//! and the TUI's panel read the same plain log the same way. The log stays plain
//! for `grep`, `--problems` and `activity`; each front end paints the roles.

pub const UPLOADED: &str = "uploaded";
pub const DELETED: &str = "deleted";
pub const FAILED: &str = "error";
pub const OK: &str = "ok";
pub const WARN: &str = "warn";
/// Width of `[uploaded]`, the widest badge; the printer pads to it so paths line up.
pub const BADGE_WIDTH: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Timestamp,
    Uploaded,
    Deleted,
    Error,
    Ok,
    Warn,
    /// Any other bracketed word, so a one-off `[tag]` still stands out.
    Tag,
    Cartridge,
    Folder,
    File,
    Plain,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub text: String,
    pub role: Role,
}

/// Split a plain line into roled segments; joining their text yields the line
/// unchanged, so a front end can colour it without reflowing it.
pub fn segments(line: &str) -> Vec<Segment> {
    let Some((stamp, rest)) = timestamp(line) else {
        return once(line, Role::Plain);
    };
    let mut out = vec![seg(stamp, Role::Timestamp)];
    let (gap, body) = leading_spaces(rest);
    push(&mut out, gap, Role::Plain);

    match tag(body) {
        Some((label, role, after)) => {
            out.push(seg(label, role));
            match role {
                // upload/delete is followed by a path; an error by a red message.
                Role::Uploaded | Role::Deleted => detail(&mut out, after),
                Role::Error => {
                    let (gap, message) = leading_spaces(after);
                    push(&mut out, gap, Role::Plain);
                    push(&mut out, message, Role::Error);
                }
                _ => push(&mut out, after, Role::Plain),
            }
        }
        None => push(&mut out, body, Role::Plain),
    }
    out
}

/// A leading `[word]`, its role, and what follows. Any word works, so the
/// printer can badge an `[ok]` the way it badges an upload.
fn tag(body: &str) -> Option<(&str, Role, &str)> {
    let close = body.strip_prefix('[')?.find(']')? + 1;
    Some((&body[..=close], role_of(&body[1..close]), &body[close + 1..]))
}

fn role_of(word: &str) -> Role {
    match word {
        UPLOADED => Role::Uploaded,
        DELETED => Role::Deleted,
        FAILED => Role::Error,
        OK => Role::Ok,
        WARN => Role::Warn,
        _ => Role::Tag,
    }
}

/// What follows an upload/delete badge: a grouped headline or a single path.
fn detail(out: &mut Vec<Segment>, after: &str) {
    let (gap, content) = leading_spaces(after);
    push(out, gap, Role::Plain);
    // A headline opens with a count ("7 file(s)..."), a path with its cartridge.
    match content.chars().next() {
        Some(first) if first.is_ascii_digit() => push(out, content, Role::Plain),
        _ => path(out, content),
    }
}

/// The cartridge and file name carry the eye; the folders between them fade.
fn path(out: &mut Vec<Segment>, path: &str) {
    match path.split_once('/') {
        None => push(out, path, Role::Cartridge),
        Some((cartridge, _)) => {
            let (folder, name) = split_last(path);
            out.push(seg(cartridge, Role::Cartridge));
            out.push(seg(&format!("{}/", &folder[cartridge.len()..]), Role::Folder));
            out.push(seg(name, Role::File));
        }
    }
}

fn timestamp(line: &str) -> Option<(&str, &str)> {
    let bytes = line.as_bytes();
    let framed = bytes.len() >= 10 && bytes[0] == b'[' && bytes[9] == b']';
    let digits = framed
        && line[1..9]
            .bytes()
            .enumerate()
            .all(|(index, byte)| match index {
                2 | 5 => byte == b':',
                _ => byte.is_ascii_digit(),
            });
    digits.then(|| line.split_at(10))
}

fn split_last(path: &str) -> (&str, &str) {
    path.rsplit_once('/').unwrap_or(("", path))
}

fn leading_spaces(text: &str) -> (&str, &str) {
    text.split_at(text.find(|c: char| c != ' ').unwrap_or(text.len()))
}

fn seg(text: &str, role: Role) -> Segment {
    Segment {
        text: text.to_string(),
        role,
    }
}

fn once(text: &str, role: Role) -> Vec<Segment> {
    vec![seg(text, role)]
}

fn push(out: &mut Vec<Segment>, text: &str, role: Role) {
    if !text.is_empty() {
        out.push(seg(text, role));
    }
}

#[cfg(test)]
#[path = "activity_tests.rs"]
mod tests;
