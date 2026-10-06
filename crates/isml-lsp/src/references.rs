//! Find All References for a function a cartridge script exports: every `require` and
//! `module.superModule` that reaches it, along the cartridge path of each storefront.
//!
//! Read from the text, not from a JavaScript parser: a module bound to a variable, required
//! inline or destructured is followed; one passed around or reassigned is not.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::reference::Reference;
use crate::resolve::{self, Override};
use crate::workspace::{Cartridge, Workspace};

/// One identifier, in UTF-16 columns as LSP counts them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub path: PathBuf,
    /// Zero-based.
    pub line: u32,
    pub start: u32,
    pub end: u32,
}

pub struct Query<'a> {
    pub file: &'a Path,
    pub text: &'a str,
    /// Character offset of the cursor into `text`.
    pub offset: usize,
    pub include_declaration: bool,
}

/// Where the function under the cursor is used, its definition first when asked for.
/// `open` holds the editor's unsaved text, which wins over the file on disk.
pub fn find(query: &Query, workspace: &Workspace, open: &HashMap<PathBuf, &str>) -> Vec<Found> {
    let chars = code(query.file, query.text);
    let Some((name, files)) = targets_at(&chars, query, workspace) else {
        return Vec::new();
    };
    let targets: Vec<Target> = files
        .iter()
        .filter_map(|file| Target::new(file, &name, workspace, open))
        .collect();

    let mut found = Vec::new();
    if query.include_declaration {
        for target in &targets {
            if let Some(text) = read(&target.path, open) {
                let chars = code(&target.path, &text);
                let definitions = exports(&chars, &name).definitions;
                found.extend(located(&target.path, &chars, &definitions, &name));
            }
        }
    }
    // Opening thousands of files is most of the cost, so the scan is split across threads.
    let sources = workspace.sources();
    let threads = std::thread::available_parallelism().map_or(1, |count| count.get());
    let chunk = sources.len().div_ceil(threads).max(1);
    std::thread::scope(|scope| {
        let workers: Vec<_> = sources
            .chunks(chunk)
            .map(|files| {
                let (name, targets) = (&name, &targets);
                scope.spawn(move || {
                    files
                        .iter()
                        .flat_map(|source| uses_in(source, name, targets, workspace, open))
                        .collect::<Vec<Found>>()
                })
            })
            .collect();
        for worker in workers {
            found.extend(worker.join().unwrap_or_default());
        }
    });
    let mut unique = Vec::new();
    for hit in found {
        if !unique.contains(&hit) {
            unique.push(hit);
        }
    }
    unique
}

fn uses_in(
    source: &Path,
    name: &str,
    targets: &[Target],
    workspace: &Workspace,
    open: &HashMap<PathBuf, &str>,
) -> Vec<Found> {
    let Some(text) = read(source, open) else {
        return Vec::new();
    };
    if !text.contains(name) {
        return Vec::new();
    }
    let chars = code(source, &text);
    let referrer = workspace.cartridge_of(source);
    let mut positions = uses(&chars, name, |origin| {
        targets
            .iter()
            .any(|target| target.reached_by(origin, source, referrer, workspace))
    });
    if targets.iter().any(|target| target.is_file(source)) {
        positions.extend(local_uses(&chars, name));
    }
    located(source, &chars, &positions, name)
}

/// The name under the cursor and the files whose export of it is meant: the file itself
/// when the cursor is on the definition, what the receiver resolves to when it is on a use.
fn targets_at(
    chars: &[char],
    query: &Query,
    workspace: &Workspace,
) -> Option<(String, Vec<PathBuf>)> {
    let (start, name) = word_at(chars, query.offset)?;
    let spans = spans(chars);
    let here = || vec![query.file.to_path_buf()];

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
                return Some((name, here()));
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
            return Some((name, here()));
        }
        return Some((name, origin_files(&origin, query.file, workspace)));
    }

    if exports(chars, &name).exported {
        return Some((name, here()));
    }
    let origin = spans.iter().find_map(|span| match &span.bound {
        Bound::Destructured(entries) => entries
            .iter()
            .any(|entry| entry.local == name || entry.key == name)
            .then(|| span.origin.clone()),
        _ => None,
    })?;
    let key = destructured_key(&spans, &name).unwrap_or(name);
    let files = origin_files(&origin, query.file, workspace);
    Some((key, files))
}

/// `var { renamed: original } = require(...)`: the export is `original`.
fn destructured_key(spans: &[Span], name: &str) -> Option<String> {
    spans.iter().find_map(|span| match &span.bound {
        Bound::Destructured(entries) => entries
            .iter()
            .find(|entry| entry.local == name || entry.key == name)
            .map(|entry| entry.key.clone()),
        _ => None,
    })
}

fn origin_files(origin: &Origin, file: &Path, workspace: &Workspace) -> Vec<PathBuf> {
    match origin {
        Origin::Require(spec) => {
            resolve::resolve(&Reference::Module(spec.clone()), file, workspace)
                .into_iter()
                .map(|hit| hit.path)
                .collect()
        }
        Origin::Super => parents(file, workspace),
    }
}

/// What `module.superModule` is in `file`: the next copy to its right in each path that
/// holds its cartridge, or every other copy when no path does.
fn parents(file: &Path, workspace: &Workspace) -> Vec<PathBuf> {
    let Some(cartridge) = workspace.cartridge_of(file) else {
        return Vec::new();
    };
    let Ok(relative) = file.strip_prefix(&cartridge.root) else {
        return Vec::new();
    };
    let others: Vec<Override> = resolve::path_modules(&relative.to_string_lossy(), file, workspace)
        .into_iter()
        .filter(|copy| copy.cartridge != cartridge.name)
        .collect();
    let holding: Vec<_> = workspace
        .paths
        .iter()
        .filter_map(|path| path.rank(&cartridge.name).map(|rank| (path, rank)))
        .collect();
    if holding.is_empty() {
        return others.into_iter().map(|copy| copy.hit.path).collect();
    }
    let mut files: Vec<PathBuf> = Vec::new();
    for (path, own) in holding {
        let next = others
            .iter()
            .filter_map(|copy| path.rank(&copy.cartridge).map(|rank| (rank, copy)))
            .filter(|(rank, _)| *rank > own)
            .min_by_key(|(rank, _)| *rank);
        if let Some((_, copy)) = next {
            if !files.contains(&copy.hit.path) {
                files.push(copy.hit.path.clone());
            }
        }
    }
    files
}

/// One file whose export is being looked for, with every cartridge's copy of it: a copy
/// left of it that defines the same name hides it from whatever reaches that copy first.
struct Target {
    path: PathBuf,
    cartridge: Cartridge,
    relative: PathBuf,
    /// Each copy, and whether it defines the name itself rather than passing the parent's on.
    copies: Vec<(Override, bool)>,
}

impl Target {
    fn new(
        path: &Path,
        name: &str,
        workspace: &Workspace,
        open: &HashMap<PathBuf, &str>,
    ) -> Option<Target> {
        let cartridge = workspace.cartridge_of(path)?.clone();
        let relative = path.strip_prefix(&cartridge.root).ok()?.to_path_buf();
        let copies = resolve::path_modules(&relative.to_string_lossy(), path, workspace)
            .into_iter()
            .map(|copy| {
                let defines = read(&copy.hit.path, open)
                    .is_some_and(|text| exports(&code(&copy.hit.path, &text), name).exported);
                (copy, defines)
            })
            .collect();
        Some(Target {
            path: path.to_path_buf(),
            cartridge,
            relative,
            copies,
        })
    }

    fn reached_by(
        &self,
        origin: &Origin,
        source: &Path,
        referrer: Option<&Cartridge>,
        workspace: &Workspace,
    ) -> bool {
        match origin {
            Origin::Super => referrer.is_some_and(|referrer| {
                referrer.name != self.cartridge.name
                    && source.strip_prefix(&referrer.root).ok() == Some(self.relative.as_path())
                    && self.along_paths(&referrer.name, true, workspace)
            }),
            Origin::Require(spec) => {
                if spec.starts_with("./") || spec.starts_with("../") {
                    let base = source.parent().unwrap_or(source);
                    return self.is(&base.join(spec));
                }
                if let Some(rest) = spec.strip_prefix("*/") {
                    let referrer = referrer.map_or("", |cartridge| cartridge.name.as_str());
                    return self.is(&self.cartridge.root.join(rest))
                        && self.along_paths(referrer, false, workspace);
                }
                if let Some(rest) = spec.strip_prefix("~/") {
                    return referrer.is_some_and(|referrer| referrer.name == self.cartridge.name)
                        && self.is(&self.cartridge.root.join(rest));
                }
                match spec.split_once('/') {
                    Some((cartridge, rest)) => {
                        cartridge == self.cartridge.name && self.is(&self.cartridge.root.join(rest))
                    }
                    None => false,
                }
            }
        }
    }

    fn is(&self, candidate: &Path) -> bool {
        resolve::existing_module(candidate).is_some_and(|module| self.is_file(&module))
    }

    fn is_file(&self, path: &Path) -> bool {
        same_file(path, &self.path)
    }

    /// Whether some path running `referrer` reaches this copy: walking it from the start —
    /// or, for `module.superModule`, from just right of the referrer — no copy before this
    /// one defines the name. With no order recorded for either cartridge, it might.
    fn along_paths(&self, referrer: &str, after_referrer: bool, workspace: &Workspace) -> bool {
        if workspace
            .paths
            .iter()
            .all(|path| path.rank(&self.cartridge.name).is_none())
        {
            return true;
        }
        let running: Vec<_> = workspace
            .paths
            .iter()
            .filter(|path| path.rank(referrer).is_some())
            .collect();
        if running.is_empty() {
            return true;
        }
        running.into_iter().any(|path| {
            let Some(own) = path.rank(&self.cartridge.name) else {
                return false;
            };
            let start = match (after_referrer, path.rank(referrer)) {
                (true, Some(rank)) => rank + 1,
                _ => 0,
            };
            own >= start
                && !self.copies.iter().any(|(copy, defines)| {
                    *defines
                        && path
                            .rank(&copy.cartridge)
                            .is_some_and(|rank| rank >= start && rank < own)
                })
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Origin {
    Require(String),
    Super,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    key: String,
    key_at: usize,
    local: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Bound {
    Nothing,
    /// `var helpers = require(...)`
    Alias(String),
    /// `var { a, b: c } = require(...)`
    Destructured(Vec<Entry>),
}

/// A `require('...')` or `module.superModule` expression, and what it is assigned to.
#[derive(Debug)]
struct Span {
    origin: Origin,
    end: usize,
    bound: Bound,
}

fn spans(chars: &[char]) -> Vec<Span> {
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
fn require_call(chars: &[char], after: usize) -> Option<(String, usize)> {
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

fn bound_before(chars: &[char], start: usize) -> Bound {
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

fn destructured(chars: &[char], close: usize) -> Bound {
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

/// Uses of `name` through every require or superModule in the file that `reaches` accepts.
fn uses(chars: &[char], name: &str, reaches: impl Fn(&Origin) -> bool) -> Vec<usize> {
    let mut positions = Vec::new();
    let mut verdicts: HashMap<Origin, bool> = HashMap::new();
    for span in spans(chars) {
        let reached = *verdicts
            .entry(span.origin.clone())
            .or_insert_with(|| reaches(&span.origin));
        if !reached {
            continue;
        }
        if let Some(at) = member_after(chars, span.end, name) {
            positions.push(at);
        }
        match &span.bound {
            Bound::Alias(alias) => {
                for at in words(chars, alias) {
                    if at > 0 && chars[at - 1] == '.' {
                        continue;
                    }
                    let after = at + alias.chars().count();
                    if chars.get(after) != Some(&'.') {
                        continue;
                    }
                    if let Some(member) = member_after(chars, after, name) {
                        positions.push(member);
                    }
                }
            }
            Bound::Destructured(entries) => {
                for entry in entries.iter().filter(|entry| entry.key == name) {
                    positions.push(entry.key_at);
                    positions.extend(
                        words(chars, &entry.local)
                            .into_iter()
                            .filter(|at| *at >= span.end && is_bare(chars, *at, &entry.local)),
                    );
                }
            }
            Bound::Nothing => {}
        }
    }
    positions
}

/// `.name` right after `end`, unless it is assigned: that is a definition, not a use.
fn member_after(chars: &[char], end: usize, name: &str) -> Option<usize> {
    let dot = skip_space(chars, end);
    if chars.get(dot) != Some(&'.') {
        return None;
    }
    let at = skip_space(chars, dot + 1);
    let member = ident_at(chars, at)?;
    (member == name && !is_assignment(chars, at + member.chars().count())).then_some(at)
}

/// A plain call or mention of `name` inside the file that defines it.
fn local_uses(chars: &[char], name: &str) -> Vec<usize> {
    let exported = exports(chars, name);
    let assigned: Vec<usize> = exported
        .definitions
        .iter()
        .filter_map(|at| assigned_value(chars, at + name.chars().count()))
        .collect();
    words(chars, name)
        .into_iter()
        .filter(|at| !exported.definitions.contains(at) && !assigned.contains(at))
        .filter(|at| {
            !exported
                .literal
                .is_some_and(|(open, close)| *at > open && *at < close)
        })
        .filter(|at| is_bare(chars, *at, name))
        .filter(|at| {
            let before = skip_space_back(chars, *at);
            !matches!(
                ident_ending_at(chars, before)
                    .map(|(_, word)| word)
                    .as_deref(),
                Some("function" | "var" | "let" | "const")
            )
        })
        .collect()
}

/// Where the value starts in `exports.name = value`.
fn assigned_value(chars: &[char], after: usize) -> Option<usize> {
    let equals = skip_space(chars, after);
    is_assignment(chars, equals).then(|| skip_space(chars, equals + 1))
}

/// Neither a member of something else nor an object key.
fn is_bare(chars: &[char], at: usize, name: &str) -> bool {
    let before = skip_space_back(chars, at);
    if before > 0 && chars[before - 1] == '.' {
        return false;
    }
    let after = skip_space(chars, at + name.chars().count());
    chars.get(after) != Some(&':')
}

struct Exports {
    /// Whether the file defines `name` itself, as opposed to passing its parent's on.
    exported: bool,
    /// Where: the exported property, and the `function` it names.
    definitions: Vec<usize>,
    /// `module.exports = { ... }`, braces included.
    literal: Option<(usize, usize)>,
}

fn exports(chars: &[char], name: &str) -> Exports {
    let spans = spans(chars);
    let parents: Vec<&str> = spans
        .iter()
        .filter(|span| span.origin == Origin::Super)
        .filter_map(|span| match &span.bound {
            Bound::Alias(alias) => Some(alias.as_str()),
            _ => None,
        })
        .collect();
    let mut definitions = Vec::new();

    // `exports.name =`, `module.exports.name =` and an override's `base.name =`.
    for at in words(chars, name) {
        let before = skip_space_back(chars, at);
        if before == 0 || chars[before - 1] != '.' {
            continue;
        }
        let Some((_, receiver)) = ident_ending_at(chars, skip_space_back(chars, before - 1)) else {
            continue;
        };
        if (receiver == "exports" || parents.contains(&receiver.as_str()))
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
fn passes_parent_on(chars: &[char], after_key: usize, name: &str, parents: &[&str]) -> bool {
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

fn exports_literal(chars: &[char]) -> Option<(usize, usize)> {
    words(chars, "exports").into_iter().find_map(|at| {
        if !preceded_by_module(chars, at) {
            return None;
        }
        let equals = skip_space(chars, at + "exports".len());
        if !is_assignment(chars, equals) {
            return None;
        }
        let open = skip_space(chars, equals + 1);
        (chars.get(open) == Some(&'{')).then_some(open)?;
        matching_brace(chars, open).map(|close| (open, close))
    })
}

fn matching_brace(chars: &[char], open: usize) -> Option<usize> {
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
fn literal_keys(chars: &[char], open: usize, close: usize) -> Vec<(usize, String)> {
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
fn skip_group(chars: &[char], open: usize) -> Option<usize> {
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
fn code(path: &Path, text: &str) -> Vec<char> {
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

fn located(path: &Path, chars: &[char], positions: &[usize], name: &str) -> Vec<Found> {
    let mut sorted = positions.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let width: u32 = name.chars().map(|c| c.len_utf16() as u32).sum();
    let mut found = Vec::new();
    let (mut line, mut column, mut index) = (0u32, 0u32, 0usize);
    for at in sorted {
        while index < at && index < chars.len() {
            if chars[index] == '\n' {
                line += 1;
                column = 0;
            } else {
                column += chars[index].len_utf16() as u32;
            }
            index += 1;
        }
        found.push(Found {
            path: path.to_path_buf(),
            line,
            start: column,
            end: column + width,
        });
    }
    found
}

fn read(path: &Path, open: &HashMap<PathBuf, &str>) -> Option<String> {
    match open.get(path) {
        Some(text) => Some(text.to_string()),
        None => fs::read_to_string(path).ok(),
    }
}

/// `..` in a relative require, or a drive letter spelt differently, still names the same file.
/// Only a file of the same name is worth asking the file system about.
fn same_file(a: &Path, b: &Path) -> bool {
    a == b
        || a.file_name() == b.file_name()
            && matches!(
                (fs::canonicalize(a), fs::canonicalize(b)),
                (Ok(a), Ok(b)) if a == b
            )
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// Starts of `word` as a whole identifier.
fn words(chars: &[char], word: &str) -> Vec<usize> {
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

fn word_at(chars: &[char], offset: usize) -> Option<(usize, String)> {
    let offset = offset.min(chars.len());
    let mut start = offset;
    while start > 0 && is_ident(chars[start - 1]) {
        start -= 1;
    }
    let word = ident_at(chars, start)?;
    (start + word.chars().count() >= offset).then_some((start, word))
}

fn ident_at(chars: &[char], start: usize) -> Option<String> {
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
fn ident_ending_at(chars: &[char], end: usize) -> Option<(usize, String)> {
    let mut start = end;
    while start > 0 && is_ident(chars[start - 1]) {
        start -= 1;
    }
    (start < end)
        .then(|| ident_at(chars, start).map(|word| (start, word)))
        .flatten()
}

fn preceded_by_module(chars: &[char], at: usize) -> bool {
    let prefix: Vec<char> = "module.".chars().collect();
    at >= prefix.len()
        && chars[at - prefix.len()..at] == prefix[..]
        && (at == prefix.len() || !is_ident(chars[at - prefix.len() - 1]))
}

fn is_assignment(chars: &[char], after: usize) -> bool {
    let at = skip_space(chars, after);
    chars.get(at) == Some(&'=') && !matches!(chars.get(at + 1), Some('=' | '>'))
}

fn skip_space(chars: &[char], mut index: usize) -> usize {
    while index < chars.len() && chars[index].is_whitespace() {
        index += 1;
    }
    index
}

/// The exclusive end once trailing whitespace before `end` is dropped.
fn skip_space_back(chars: &[char], mut end: usize) -> usize {
    while end > 0 && chars[end - 1].is_whitespace() {
        end -= 1;
    }
    end
}

#[cfg(test)]
mod tests {
    use super::*;

    const HELPER: &str = "cartridge/scripts/helpers/cartHelpers.js";
    const CART: &str = "cartridge/controllers/Cart.js";
    const REQUIRE: &str = "require('*/cartridge/scripts/helpers/cartHelpers')";

    const DEFINES: &str = "'use strict';\nfunction total(basket) {\n    return basket;\n}\nfunction other() {\n    return total(null);\n}\nmodule.exports = {\n    total: total,\n    other: other\n};\n";
    const REDEFINES: &str = "var base = module.superModule;\nbase.total = function (basket) {\n    return base.total(basket);\n};\nmodule.exports = base;\n";
    const PASSES_ON: &str =
        "var base = module.superModule;\nmodule.exports = {\n    total: base.total\n};\n";

    /// `site_a` runs `app_na:app_brand:app_storefront_base`, `site_b` leaves `app_na` out.
    fn checkout(name: &str, files: &[(&str, &str, &str)]) -> PathBuf {
        let root = std::env::temp_dir().join(name);
        let _ = fs::remove_dir_all(&root);
        for (cartridge, file, text) in files {
            let path = root.join("cartridges").join(cartridge).join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, text).unwrap();
        }
        root
    }

    fn workspace(root: &Path) -> Workspace {
        let settings = serde_json::json!({
            "cartridge_path": {
                "site_a": "app_na:app_brand:app_storefront_base",
                "site_b": "app_brand:app_storefront_base",
            }
        });
        Workspace::scan(&[root.to_path_buf()], &settings)
    }

    /// References with the cursor on the first `at` in the file, as `cartridge line`.
    fn references(workspace: &Workspace, root: &Path, file: (&str, &str), at: &str) -> Vec<String> {
        let path = root.join("cartridges").join(file.0).join(file.1);
        let text = fs::read_to_string(&path).unwrap();
        let offset = text[..text.find(at).unwrap()].chars().count();
        let query = Query {
            file: &path,
            text: &text,
            offset,
            include_declaration: false,
        };
        let mut found: Vec<String> = find(&query, workspace, &HashMap::new())
            .into_iter()
            .map(|hit| {
                let relative = hit.path.strip_prefix(root.join("cartridges")).unwrap();
                let cartridge = relative.iter().next().unwrap().to_string_lossy();
                format!("{cartridge} {}", hit.line)
            })
            .collect();
        found.sort();
        found
    }

    fn uses() -> String {
        format!(
            "var helpers = {REQUIRE};\nhelpers.total(1);\n{REQUIRE}.total(2);\nvar {{ total }} = {REQUIRE};\ntotal(3);\n// helpers.total(4);\nhelpers.totalPrice(5);\n"
        )
    }

    #[test]
    fn finds_every_way_of_importing_the_function() {
        let cart = uses();
        let template = format!("<isprint value=\"${{{REQUIRE}.total(pdict.basket)}}\"/>\n");
        let root = checkout(
            "isml-lsp-references-imports",
            &[
                ("app_storefront_base", HELPER, DEFINES),
                ("app_brand", CART, &cart),
                (
                    "app_brand",
                    "cartridge/templates/default/cart.isml",
                    &template,
                ),
            ],
        );
        let found = references(
            &workspace(&root),
            &root,
            ("app_storefront_base", HELPER),
            "total(basket)",
        );
        assert_eq!(
            found,
            [
                "app_brand 0",
                "app_brand 1",
                "app_brand 2",
                "app_brand 3",
                "app_brand 4",
                "app_storefront_base 5"
            ]
        );
    }

    #[test]
    fn answers_the_same_from_a_use_as_from_the_definition() {
        let cart = uses();
        let root = checkout(
            "isml-lsp-references-from-use",
            &[
                ("app_storefront_base", HELPER, DEFINES),
                ("app_brand", CART, &cart),
            ],
        );
        let workspace = workspace(&root);
        let from_definition = references(
            &workspace,
            &root,
            ("app_storefront_base", HELPER),
            "total: total",
        );
        let from_use = references(&workspace, &root, ("app_brand", CART), "total(1)");
        assert_eq!(from_use, from_definition);
    }

    #[test]
    fn an_override_that_redefines_the_function_hides_the_parent() {
        let cart = uses();
        let root = checkout(
            "isml-lsp-references-hidden",
            &[
                ("app_storefront_base", HELPER, DEFINES),
                ("app_brand", HELPER, REDEFINES),
                ("app_brand", CART, &cart),
            ],
        );
        let workspace = workspace(&root);
        // Only the override's own call to its parent, and the base's internal call.
        let found = references(
            &workspace,
            &root,
            ("app_storefront_base", HELPER),
            "total(basket)",
        );
        assert_eq!(found, ["app_brand 2", "app_storefront_base 5"]);
        // The callers reach the override instead.
        let found = references(&workspace, &root, ("app_brand", HELPER), "total = function");
        assert_eq!(
            found
                .iter()
                .filter(|hit| hit.starts_with("app_brand"))
                .count(),
            4
        );
    }

    #[test]
    fn an_override_that_passes_the_function_on_does_not_hide_it() {
        let cart = uses();
        let root = checkout(
            "isml-lsp-references-passes-on",
            &[
                ("app_storefront_base", HELPER, DEFINES),
                ("app_na", HELPER, PASSES_ON),
                ("app_brand", CART, &cart),
            ],
        );
        let found = references(
            &workspace(&root),
            &root,
            ("app_storefront_base", HELPER),
            "total(basket)",
        );
        assert_eq!(
            found
                .iter()
                .filter(|hit| hit.starts_with("app_brand"))
                .count(),
            4
        );
        // `base.total` in the override hands the parent's on: a use of it.
        assert!(found.contains(&"app_na 2".to_string()));
    }

    #[test]
    fn counts_a_caller_only_in_the_storefronts_that_run_it() {
        let na_caller = format!("{REQUIRE}.total(1);\n");
        let root = checkout(
            "isml-lsp-references-storefront",
            &[
                ("app_brand", HELPER, DEFINES),
                ("app_na", HELPER, REDEFINES),
                ("app_na", "cartridge/scripts/caller.js", &na_caller),
                ("app_brand", CART, &uses()),
            ],
        );
        let found = references(
            &workspace(&root),
            &root,
            ("app_brand", HELPER),
            "total(basket)",
        );
        // The brand cart reaches it through site_b; app_na runs only in site_a, where its
        // own copy answers, so its caller does not — but its call to the parent does.
        assert!(!found.contains(&"app_na 0".to_string()));
        assert!(found.contains(&"app_na 2".to_string()));
        assert_eq!(
            found
                .iter()
                .filter(|hit| hit.starts_with("app_brand"))
                .count(),
            5
        );
    }

    #[test]
    fn a_tilde_require_names_its_own_cartridge() {
        let caller = "require('~/cartridge/scripts/helpers/cartHelpers').total(1);\n";
        let root = checkout(
            "isml-lsp-references-tilde",
            &[
                ("app_storefront_base", HELPER, DEFINES),
                ("app_brand", HELPER, REDEFINES),
                ("app_brand", "cartridge/scripts/caller.js", caller),
                ("app_storefront_base", "cartridge/scripts/caller.js", caller),
            ],
        );
        let found = references(
            &workspace(&root),
            &root,
            ("app_storefront_base", HELPER),
            "total(basket)",
        );
        assert!(found.contains(&"app_storefront_base 0".to_string()));
        assert!(!found.contains(&"app_brand 0".to_string()));
    }

    #[test]
    fn counts_every_copy_when_no_cartridge_path_is_recorded() {
        let root = checkout(
            "isml-lsp-references-no-path",
            &[
                ("app_storefront_base", HELPER, DEFINES),
                ("app_brand", HELPER, REDEFINES),
                ("app_brand", CART, &uses()),
            ],
        );
        let workspace = Workspace::scan(std::slice::from_ref(&root), &serde_json::Value::Null);
        let found = references(
            &workspace,
            &root,
            ("app_storefront_base", HELPER),
            "total(basket)",
        );
        assert_eq!(
            found
                .iter()
                .filter(|hit| hit.starts_with("app_brand"))
                .count(),
            5
        );
    }

    #[test]
    fn includes_the_definition_when_asked() {
        let root = checkout(
            "isml-lsp-references-declaration",
            &[("app_storefront_base", HELPER, DEFINES)],
        );
        let path = root.join("cartridges/app_storefront_base").join(HELPER);
        let query = Query {
            file: &path,
            text: DEFINES,
            offset: DEFINES.find("total: total").unwrap(),
            include_declaration: true,
        };
        let lines: Vec<(u32, u32)> = find(&query, &workspace(&root), &HashMap::new())
            .into_iter()
            .map(|hit| (hit.line, hit.start))
            .collect();
        assert_eq!(lines, [(1, 9), (8, 4), (5, 11)]);
    }

    #[test]
    fn the_value_an_export_is_assigned_is_not_a_use() {
        let helper = "var parent = module.superModule;\nfunction total() {}\nmodule.exports = parent;\nmodule.exports.total = total;\n";
        let root = checkout(
            "isml-lsp-references-assigned",
            &[("app_brand", HELPER, helper)],
        );
        let found = references(&workspace(&root), &root, ("app_brand", HELPER), "total()");
        assert!(found.is_empty(), "{found:?}");
    }

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
}
