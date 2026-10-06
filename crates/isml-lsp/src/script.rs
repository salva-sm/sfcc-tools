//! JavaScript read as text, the way SFCC scripts use it: what a file requires, binds to
//! `module.superModule` and exports. No parser: enough for the shapes cartridge code takes.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum Origin {
    Require(String),
    Super,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Entry {
    pub(crate) key: String,
    pub(crate) key_at: usize,
    pub(crate) local: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Bound {
    Nothing,
    /// `var helpers = require(...)`
    Alias(String),
    /// `var { a, b: c } = require(...)`
    Destructured(Vec<Entry>),
}

/// A `require('...')` or `module.superModule` expression, and what it is assigned to.
#[derive(Debug)]
pub(crate) struct Span {
    pub(crate) origin: Origin,
    pub(crate) end: usize,
    pub(crate) bound: Bound,
}

pub(crate) fn spans(chars: &[char]) -> Vec<Span> {
    let mut found = Vec::new();
    for at in words(chars, "require") {
        if let Some((spec, end)) = require_call(chars, at + "require".len()) {
            found.push(Span {
                origin: Origin::Require(spec),
                end,
                bound: bound_before(chars, at),
            });
        }
    }
    for at in words(chars, "superModule") {
        if preceded_by_module(chars, at) {
            let start = at - "module.".len();
            found.push(Span {
                origin: Origin::Super,
                end: at + "superModule".len(),
                bound: bound_before(chars, start),
            });
        }
    }
    found
}

/// `('spec')` after `require`: the spec and the index just past `)`.
pub(crate) fn require_call(chars: &[char], after: usize) -> Option<(String, usize)> {
    let open = skip_space(chars, after);
    if chars.get(open) != Some(&'(') {
        return None;
    }
    let quote_at = skip_space(chars, open + 1);
    let quote = *chars.get(quote_at)?;
    if !matches!(quote, '\'' | '"' | '`') {
        return None;
    }
    let close_quote = (quote_at + 1..chars.len()).find(|index| chars[*index] == quote)?;
    let close = skip_space(chars, close_quote + 1);
    (chars.get(close) == Some(&')')).then(|| {
        let spec = chars[quote_at + 1..close_quote].iter().collect();
        (spec, close + 1)
    })
}

pub(crate) fn bound_before(chars: &[char], start: usize) -> Bound {
    let equals = skip_space_back(chars, start);
    if equals == 0 || chars[equals - 1] != '=' {
        return Bound::Nothing;
    }
    if equals >= 2 && "=!<>+-*/%&|^".contains(chars[equals - 2]) {
        return Bound::Nothing;
    }
    let target_end = skip_space_back(chars, equals - 1);
    if target_end > 0 && chars[target_end - 1] == '}' {
        return destructured(chars, target_end - 1);
    }
    match ident_ending_at(chars, target_end) {
        // `module.exports = require(...)` re-exports; it binds nothing in this file.
        Some((at, alias)) if at == 0 || chars[at - 1] != '.' => Bound::Alias(alias),
        _ => Bound::Nothing,
    }
}

pub(crate) fn destructured(chars: &[char], close: usize) -> Bound {
    let Some(open) = (0..close).rev().find(|index| chars[*index] == '{') else {
        return Bound::Nothing;
    };
    let mut entries = Vec::new();
    let mut segment_start = open + 1;
    for index in open + 1..=close {
        if index != close && chars[index] != ',' {
            continue;
        }
        let key_at = skip_space(chars, segment_start);
        if let Some(key) = ident_at(chars, key_at) {
            let after = skip_space(chars, key_at + key.chars().count());
            let local = match chars.get(after) {
                Some(':') => ident_at(chars, skip_space(chars, after + 1)),
                _ => None,
            };
            entries.push(Entry {
                local: local.unwrap_or_else(|| key.clone()),
                key,
                key_at,
            });
        }
        segment_start = index + 1;
    }
    Bound::Destructured(entries)
}

pub(crate) struct Exports {
    /// Whether the file defines `name` itself, as opposed to passing its parent's on.
    pub(crate) exported: bool,
    /// Where: the exported property, and the `function` it names.
    pub(crate) definitions: Vec<usize>,
    /// `module.exports = { ... }`, braces included.
    pub(crate) literal: Option<(usize, usize)>,
}

pub(crate) fn exports(chars: &[char], name: &str) -> Exports {
    let parents = parent_aliases(chars);
    let parents: Vec<&str> = parents.iter().map(String::as_str).collect();
    let objects = exported_objects(chars);
    let mut definitions = Vec::new();

    // `exports.name =`, `module.exports.name =`, an override's `base.name =`, and
    // `helpers.name =` when the file ends with `module.exports = helpers`.
    for at in words(chars, name) {
        let before = skip_space_back(chars, at);
        if before == 0 || chars[before - 1] != '.' {
            continue;
        }
        let Some((_, receiver)) = ident_ending_at(chars, skip_space_back(chars, before - 1)) else {
            continue;
        };
        if (receiver == "exports"
            || parents.contains(&receiver.as_str())
            || objects.contains(&receiver))
            && is_assignment(chars, at + name.chars().count())
        {
            definitions.push(at);
        }
    }

    let literal = exports_literal(chars);
    if let Some((open, close)) = literal {
        for (at, key) in literal_keys(chars, open, close) {
            if key == name && !passes_parent_on(chars, at + key.chars().count(), name, &parents) {
                definitions.push(at);
            }
        }
    }

    let exported = !definitions.is_empty();
    if exported {
        for at in words(chars, name) {
            let before = skip_space_back(chars, at);
            if ident_ending_at(chars, before).is_some_and(|(_, word)| word == "function") {
                definitions.push(at);
            }
        }
    }
    Exports {
        exported,
        definitions,
        literal,
    }
}

/// `name: base.name` hands the parent's function on unchanged.
pub(crate) fn passes_parent_on(
    chars: &[char],
    after_key: usize,
    name: &str,
    parents: &[&str],
) -> bool {
    let colon = skip_space(chars, after_key);
    if chars.get(colon) != Some(&':') {
        return false;
    }
    let value_at = skip_space(chars, colon + 1);
    let Some(receiver) = ident_at(chars, value_at) else {
        return false;
    };
    if !parents.contains(&receiver.as_str()) {
        return false;
    }
    let dot = value_at + receiver.chars().count();
    if chars.get(dot) != Some(&'.') || ident_at(chars, dot + 1).as_deref() != Some(name) {
        return false;
    }
    let end = skip_space(chars, dot + 1 + name.chars().count());
    matches!(chars.get(end), None | Some(',' | '}'))
}

/// The aliases of `module.superModule`: `var base = module.superModule`.
pub(crate) fn parent_aliases(chars: &[char]) -> Vec<String> {
    spans(chars)
        .into_iter()
        .filter(|span| span.origin == Origin::Super)
        .filter_map(|span| match span.bound {
            Bound::Alias(alias) => Some(alias),
            _ => None,
        })
        .collect()
}

/// Where each value assigned to `module.exports` starts.
fn export_values(chars: &[char]) -> Vec<usize> {
    words(chars, "exports")
        .into_iter()
        .filter(|at| preceded_by_module(chars, *at))
        .filter_map(|at| {
            let equals = skip_space(chars, at + "exports".len());
            is_assignment(chars, equals).then(|| skip_space(chars, equals + 1))
        })
        .collect()
}

/// `module.exports = helpers`: the object whose members are the exports.
pub(crate) fn exported_objects(chars: &[char]) -> Vec<String> {
    export_values(chars)
        .into_iter()
        .filter_map(|value| {
            let object = ident_at(chars, value)?;
            let after = skip_space(chars, value + object.chars().count());
            (!matches!(chars.get(after), Some('.' | '('))).then_some(object)
        })
        .collect()
}

/// The value `object` is declared with, up to the end of its statement.
fn declared_value(chars: &[char], object: &str) -> Option<(usize, usize)> {
    words(chars, object).into_iter().find_map(|at| {
        let before = skip_space_back(chars, at);
        if before > 0 && chars[before - 1] == '.' {
            return None;
        }
        let equals = skip_space(chars, at + object.chars().count());
        if !is_assignment(chars, equals) {
            return None;
        }
        let value = skip_space(chars, equals + 1);
        let end = match chars.get(value) {
            Some('{') => matching_brace(chars, value)? + 1,
            _ => (value..chars.len())
                .find(|index| chars[*index] == ';')
                .unwrap_or(chars.len()),
        };
        Some((value, end))
    })
}

/// The object literal the file exports: `module.exports = { ... }`, or the one declared
/// for `var helpers = { ... }; module.exports = helpers`.
pub(crate) fn exports_literal(chars: &[char]) -> Option<(usize, usize)> {
    export_values(chars).into_iter().find_map(|value| {
        let open = match chars.get(value) {
            Some('{') => value,
            _ => {
                let object = ident_at(chars, value)?;
                let (start, _) = declared_value(chars, &object)?;
                start
            }
        };
        (chars.get(open) == Some(&'{')).then_some(open)?;
        matching_brace(chars, open).map(|close| (open, close))
    })
}

/// Whether a file with no `name` of its own still exports its parent's: `module.exports =
/// base`, an object made from it (`Object.create(base)`, `Object.assign({}, base)`), or a
/// `name: base.name` key.
pub(crate) fn inherits(chars: &[char], name: &str) -> bool {
    let parents = parent_aliases(chars);
    if parents.is_empty() {
        return false;
    }
    let parent_names: Vec<&str> = parents.iter().map(String::as_str).collect();
    if let Some((open, close)) = exports_literal(chars) {
        if literal_keys(chars, open, close)
            .into_iter()
            .any(|(at, key)| {
                key == name
                    && passes_parent_on(chars, at + key.chars().count(), name, &parent_names)
            })
        {
            return true;
        }
    }
    let mentions_parent = |(start, end): (usize, usize)| {
        parent_names
            .iter()
            .any(|alias| !words(&chars[start..end], alias).is_empty())
    };
    export_values(chars).into_iter().any(|value| {
        let end = (value..chars.len())
            .find(|index| chars[*index] == ';')
            .unwrap_or(chars.len());
        match ident_at(chars, value) {
            Some(object) if parent_names.contains(&object.as_str()) => true,
            Some(object) if object == "Object" => mentions_parent((value, end)),
            Some(object) => declared_value(chars, &object).is_some_and(mentions_parent),
            None => false,
        }
    })
}

pub(crate) fn matching_brace(chars: &[char], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    let mut index = open;
    while index < chars.len() {
        let c = chars[index];
        match quote {
            Some(_) if c == '\\' => index += 1,
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if matches!(c, '\'' | '"' | '`') => quote = Some(c),
            None if matches!(c, '{' | '(' | '[') => depth += 1,
            None if matches!(c, '}' | ')' | ']') => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(index);
                }
            }
            None => {}
        }
        index += 1;
    }
    None
}

/// Keys at the top level of the literal: `key: value`, `key,` and `key() {}`.
pub(crate) fn literal_keys(chars: &[char], open: usize, close: usize) -> Vec<(usize, String)> {
    let mut keys = Vec::new();
    let mut expecting_key = true;
    let mut index = open + 1;
    while index < close {
        let c = chars[index];
        if c.is_whitespace() {
            index += 1;
            continue;
        }
        if expecting_key {
            if let Some(key) = ident_at(chars, index) {
                keys.push((index, key.clone()));
                index += key.chars().count();
                expecting_key = false;
                continue;
            }
            expecting_key = false;
        }
        match c {
            ',' => expecting_key = true,
            '{' | '(' | '[' | '\'' | '"' | '`' => {
                index = skip_group(chars, index).unwrap_or(close);
            }
            _ => {}
        }
        index += 1;
    }
    keys
}

/// The index of what closes the bracket or string opened at `open`.
pub(crate) fn skip_group(chars: &[char], open: usize) -> Option<usize> {
    let c = chars[open];
    if matches!(c, '\'' | '"' | '`') {
        let mut index = open + 1;
        while index < chars.len() {
            match chars[index] {
                '\\' => index += 1,
                found if found == c => return Some(index),
                _ => {}
            }
            index += 1;
        }
        return None;
    }
    matching_brace(chars, open)
}

/// The text as characters, with JavaScript comments blanked so a commented-out call is not a
/// use; positions and line breaks are kept. Templates are left as they are.
pub(crate) fn code(path: &Path, text: &str) -> Vec<char> {
    let mut chars: Vec<char> = text.chars().collect();
    if path
        .extension()
        .is_none_or(|extension| extension != "js" && extension != "ds")
    {
        return chars;
    }
    let mut quote: Option<char> = None;
    let mut index = 0;
    while index < chars.len() {
        let c = chars[index];
        if let Some(q) = quote {
            if c == '\\' {
                index += 1;
            } else if c == q || (c == '\n' && q != '`') {
                quote = None;
            }
            index += 1;
            continue;
        }
        let next = chars.get(index + 1).copied();
        match (c, next) {
            ('\'' | '"' | '`', _) => quote = Some(c),
            ('/', Some('/')) => {
                while index < chars.len() && chars[index] != '\n' {
                    chars[index] = ' ';
                    index += 1;
                }
                continue;
            }
            ('/', Some('*')) => {
                // `/*/` does not close: the `*` that closes comes after the opening one.
                let mut previous = ' ';
                let mut blanked = 0;
                while index < chars.len() {
                    let current = chars[index];
                    let closing = blanked >= 3 && previous == '*' && current == '/';
                    if current != '\n' {
                        chars[index] = ' ';
                    }
                    previous = current;
                    index += 1;
                    blanked += 1;
                    if closing {
                        break;
                    }
                }
                continue;
            }
            _ => {}
        }
        index += 1;
    }
    chars
}

/// Where the name under the cursor comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Member {
    /// Defined by this file: `exports.name =`, a key of the exported literal, a function
    /// the file exports, or an override's `base.name =`.
    Here,
    /// Exported by what the origin names: `alias.name`, `require(...).name`, `base.name`,
    /// or a name destructured from a require.
    Of(Origin),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MemberAt {
    /// The exported name, which for `var { total: sum }` is `total` even on `sum`.
    pub(crate) name: String,
    pub(crate) member: Member,
}

/// The exported function the identifier at `offset` names, if it names one.
pub(crate) fn member_at(chars: &[char], offset: usize) -> Option<MemberAt> {
    let (start, name) = word_at(chars, offset)?;
    let spans = spans(chars);
    let here = |name: String| {
        Some(MemberAt {
            name,
            member: Member::Here,
        })
    };

    let before = skip_space_back(chars, start);
    if before > 0 && chars[before - 1] == '.' {
        let receiver_end = skip_space_back(chars, before - 1);
        let origin = if receiver_end > 0 && chars[receiver_end - 1] == ')' {
            spans
                .iter()
                .find(|span| span.end == receiver_end)
                .map(|span| span.origin.clone())?
        } else {
            let (receiver_start, receiver) = ident_ending_at(chars, receiver_end)?;
            if receiver == "exports" {
                return here(name);
            }
            if receiver == "superModule" && preceded_by_module(chars, receiver_start) {
                Origin::Super
            } else {
                spans
                    .iter()
                    .find(|span| matches!(&span.bound, Bound::Alias(alias) if *alias == receiver))
                    .map(|span| span.origin.clone())?
            }
        };
        // `base.foo = function` is this file overriding `foo`, not a use of the parent's.
        if origin == Origin::Super && is_assignment(chars, start + name.chars().count()) {
            return here(name);
        }
        return Some(MemberAt {
            name,
            member: Member::Of(origin),
        });
    }

    if exports(chars, &name).exported {
        return here(name);
    }
    // `var { renamed: original } = require(...)`: the export is `original`.
    spans.iter().find_map(|span| match &span.bound {
        Bound::Destructured(entries) => entries
            .iter()
            .find(|entry| entry.local == name || entry.key == name)
            .map(|entry| MemberAt {
                name: entry.key.clone(),
                member: Member::Of(span.origin.clone()),
            }),
        _ => None,
    })
}

/// Zero-based line and UTF-16 column of a character offset, as LSP counts them.
pub(crate) fn line_column(chars: &[char], at: usize) -> (u32, u32) {
    let (mut line, mut column) = (0u32, 0u32);
    for c in &chars[..at.min(chars.len())] {
        if *c == '\n' {
            line += 1;
            column = 0;
        } else {
            column += c.len_utf16() as u32;
        }
    }
    (line, column)
}

pub(crate) fn read(path: &Path, open: &HashMap<PathBuf, &str>) -> Option<String> {
    match open.get(path) {
        Some(text) => Some(text.to_string()),
        None => fs::read_to_string(path).ok(),
    }
}

/// `..` in a relative require, or a drive letter spelt differently, still names the same file.
/// Only a file of the same name is worth asking the file system about.
pub(crate) fn same_file(a: &Path, b: &Path) -> bool {
    a == b
        || a.file_name() == b.file_name()
            && matches!(
                (fs::canonicalize(a), fs::canonicalize(b)),
                (Ok(a), Ok(b)) if a == b
            )
}

pub(crate) fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// Starts of `word` as a whole identifier.
pub(crate) fn words(chars: &[char], word: &str) -> Vec<usize> {
    let needle: Vec<char> = word.chars().collect();
    if needle.is_empty() || chars.len() < needle.len() {
        return Vec::new();
    }
    (0..=chars.len() - needle.len())
        .filter(|start| chars[*start..*start + needle.len()] == needle[..])
        .filter(|start| *start == 0 || !is_ident(chars[*start - 1]))
        .filter(|start| {
            chars
                .get(start + needle.len())
                .is_none_or(|c| !is_ident(*c))
        })
        .collect()
}

pub(crate) fn word_at(chars: &[char], offset: usize) -> Option<(usize, String)> {
    let offset = offset.min(chars.len());
    let mut start = offset;
    while start > 0 && is_ident(chars[start - 1]) {
        start -= 1;
    }
    let word = ident_at(chars, start)?;
    (start + word.chars().count() >= offset).then_some((start, word))
}

pub(crate) fn ident_at(chars: &[char], start: usize) -> Option<String> {
    let first = *chars.get(start)?;
    if !is_ident(first) || first.is_ascii_digit() {
        return None;
    }
    Some(
        chars[start..]
            .iter()
            .take_while(|c| is_ident(**c))
            .collect(),
    )
}

/// The identifier whose last character is just before `end`.
pub(crate) fn ident_ending_at(chars: &[char], end: usize) -> Option<(usize, String)> {
    let mut start = end;
    while start > 0 && is_ident(chars[start - 1]) {
        start -= 1;
    }
    (start < end)
        .then(|| ident_at(chars, start).map(|word| (start, word)))
        .flatten()
}

pub(crate) fn preceded_by_module(chars: &[char], at: usize) -> bool {
    let prefix: Vec<char> = "module.".chars().collect();
    at >= prefix.len()
        && chars[at - prefix.len()..at] == prefix[..]
        && (at == prefix.len() || !is_ident(chars[at - prefix.len() - 1]))
}

pub(crate) fn is_assignment(chars: &[char], after: usize) -> bool {
    let at = skip_space(chars, after);
    chars.get(at) == Some(&'=') && !matches!(chars.get(at + 1), Some('=' | '>'))
}

pub(crate) fn skip_space(chars: &[char], mut index: usize) -> usize {
    while index < chars.len() && chars[index].is_whitespace() {
        index += 1;
    }
    index
}

/// The exclusive end once trailing whitespace before `end` is dropped.
pub(crate) fn skip_space_back(chars: &[char], mut end: usize) -> usize {
    while end > 0 && chars[end - 1].is_whitespace() {
        end -= 1;
    }
    end
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_keys_of_the_exported_literal() {
        let chars: Vec<char> =
            "module.exports = {\n    a: function () { return { b: 1 }; },\n    b,\n    c() {}\n};"
                .chars()
                .collect();
        let (open, close) = exports_literal(&chars).unwrap();
        let keys: Vec<String> = literal_keys(&chars, open, close)
            .into_iter()
            .map(|(_, key)| key)
            .collect();
        assert_eq!(keys, ["a", "b", "c"]);
    }

    #[test]
    fn blanks_comments_but_not_strings() {
        let text = "a('//x'); // b\n/* c */ d";
        let masked: String = code(Path::new("x.js"), text).into_iter().collect();
        assert_eq!(masked, "a('//x');     \n        d");
    }

    fn chars(text: &str) -> Vec<char> {
        text.chars().collect()
    }

    #[test]
    fn reads_the_members_of_an_exported_object() {
        let text = chars(
            "var helpers = {};\nhelpers.total = function () {};\nmodule.exports = helpers;\n",
        );
        assert!(exports(&text, "total").exported);
        assert!(!exports(&text, "other").exported);
    }

    #[test]
    fn reads_a_literal_declared_before_it_is_exported() {
        let text = chars("var helpers = {\n    total: total\n};\nmodule.exports = helpers;\n");
        assert!(exports(&text, "total").exported);
    }

    #[test]
    fn an_object_made_from_the_parent_inherits_what_it_does_not_define() {
        let text = chars("var base = module.superModule;\nvar helpers = Object.create(base);\nhelpers.total = function () {};\nmodule.exports = helpers;\n");
        assert!(exports(&text, "total").exported);
        assert!(inherits(&text, "other"));

        let assigned = chars(
            "var base = module.superModule;\nmodule.exports = Object.assign({}, base, { a: 1 });\n",
        );
        assert!(inherits(&assigned, "other"));

        let unrelated = chars("var base = module.superModule;\nmodule.exports = { a: 1 };\n");
        assert!(!inherits(&unrelated, "other"));
    }
}
