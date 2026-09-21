//! The Demandware script API, compiled in.
//!
//! `require('dw/system/Site')` and what `Site` then has on it are the two
//! things an editor cannot answer about SFCC script: there is no `.d.ts` in
//! the checkout and no package to resolve. The index is generated from the
//! platform reference by `examples/generate-api.rs` and embedded, so an answer
//! never depends on a `node_modules` being present.

use std::collections::BTreeMap;
use std::sync::OnceLock;

const INDEX: &str = include_str!("api.json");

/// One `dw.*` class, as the platform reference describes it.
#[derive(Debug, Default, serde::Deserialize)]
pub struct Class {
    /// What the class is for, trimmed to fit a popup.
    #[serde(rename = "d", default)]
    pub description: String,
    /// Values the class exposes as named constants.
    #[serde(rename = "c", default)]
    pub constants: Vec<Member>,
    /// Readable fields, most of them read-only.
    #[serde(rename = "p", default)]
    pub properties: Vec<Member>,
    /// Callable members, static ones included.
    #[serde(rename = "m", default)]
    pub methods: Vec<Member>,
}

/// One constant, property or method of a class.
#[derive(Debug, serde::Deserialize)]
pub struct Member {
    /// The identifier, as it is written after the dot.
    #[serde(rename = "n")]
    pub name: String,
    /// A type for a property or constant, a signature for a method.
    #[serde(rename = "s", default)]
    pub shape: String,
    /// What the member does.
    #[serde(rename = "d", default)]
    pub description: String,
}

/// The whole API, keyed by qualified class name.
#[derive(Debug, Default)]
pub struct Api {
    classes: BTreeMap<String, Class>,
}

/// Parsed once, on the first question — a session that never asks about the
/// API never pays for it.
pub fn api() -> &'static Api {
    static API: OnceLock<Api> = OnceLock::new();
    API.get_or_init(|| Api {
        classes: serde_json::from_str(INDEX).unwrap_or_default(),
    })
}

impl Api {
    /// A class by its qualified name, `dw.system.Site`.
    pub fn class(&self, qualified: &str) -> Option<&Class> {
        self.classes.get(qualified)
    }

    /// `dw/system/Site`, as a `require` spells it. `TopLevel` classes are left
    /// out: they are globals, not modules.
    pub fn modules(&self) -> impl Iterator<Item = (String, &Class)> {
        self.classes
            .iter()
            .filter(|(name, _)| name.starts_with("dw."))
            .map(|(name, class)| (name.replace('.', "/"), class))
    }

    /// A class by the path a `require` gives, `dw/system/Site`.
    pub fn by_module(&self, path: &str) -> Option<&Class> {
        self.class(&path.trim_matches('/').replace('/', "."))
    }
}

impl Class {
    /// Everything reachable through the dot, in the order a reader wants it.
    pub fn members(&self) -> impl Iterator<Item = (&Member, MemberKind)> {
        let methods = self
            .methods
            .iter()
            .map(|member| (member, MemberKind::Method));
        let properties = self
            .properties
            .iter()
            .map(|member| (member, MemberKind::Property));
        let constants = self
            .constants
            .iter()
            .map(|member| (member, MemberKind::Constant));
        methods.chain(properties).chain(constants)
    }

    /// One member by name, whatever kind it is.
    pub fn member(&self, name: &str) -> Option<(&Member, MemberKind)> {
        self.members().find(|(member, _)| member.name == name)
    }
}

/// What a member is, which decides the icon the editor shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberKind {
    /// A callable member.
    Method,
    /// A readable field.
    Property,
    /// A named constant.
    Constant,
}

/// `var Site = require('dw/system/Site');` — the identifiers in a document
/// that stand for an API class, and which class each one is.
pub fn bindings(text: &str) -> BTreeMap<String, String> {
    const CALL: &str = "require(";
    let mut found = BTreeMap::new();
    for line in text.lines() {
        let Some(call) = line.find(CALL) else {
            continue;
        };
        let Some(module) = quoted(&line[call + CALL.len()..]) else {
            continue;
        };
        if !module.starts_with("dw/") {
            continue;
        }
        if let Some(name) = assigned_name(&line[..call]) {
            found.insert(name, module.replace('/', "."));
        }
    }
    found
}

/// The identifier a `var x = ` on the left of the call declares.
fn assigned_name(before: &str) -> Option<String> {
    let head = before.trim_end().strip_suffix('=')?.trim_end();
    let name: String = head
        .chars()
        .rev()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    (!name.is_empty()).then_some(name)
}

fn quoted(text: &str) -> Option<String> {
    let trimmed = text.trim_start();
    let quote = trimmed.chars().next()?;
    if quote != '\'' && quote != '"' {
        return None;
    }
    let body = &trimmed[quote.len_utf8()..];
    let end = body.find(quote)?;
    Some(body[..end].to_string())
}

/// The `Site.getCurrent` under the cursor, resolved through the document's
/// bindings to a class and one of its members.
pub fn member_at(line: &str, column: usize, text: &str) -> Option<(String, String)> {
    let chars: Vec<char> = line.chars().collect();
    let column = column.min(chars.len().saturating_sub(1));

    let mut start = column;
    while start > 0 && is_name(chars[start - 1]) {
        start -= 1;
    }
    let mut end = column;
    while end < chars.len() && is_name(chars[end]) {
        end += 1;
    }
    if start == end || start == 0 || chars[start - 1] != '.' {
        return None;
    }

    let mut receiver_start = start - 1;
    while receiver_start > 0 && is_name(chars[receiver_start - 1]) {
        receiver_start -= 1;
    }
    let receiver: String = chars[receiver_start..start - 1].iter().collect();
    let class = bindings(text).get(&receiver)?.clone();
    Some((class, chars[start..end].iter().collect()))
}

fn is_name(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_embedded_index_parses_and_holds_the_platform() {
        let api = api();
        assert!(api.class("dw.system.Site").is_some());
        assert!(api.class("dw.catalog.ProductMgr").is_some());
        assert!(api.modules().count() > 300);
    }

    #[test]
    fn resolves_a_module_path_the_way_require_spells_it() {
        assert!(api().by_module("dw/web/URLUtils").is_some());
        assert!(api().by_module("dw/nope/Nope").is_none());
    }

    #[test]
    fn finds_a_method_with_its_signature() {
        let (member, kind) = api()
            .class("dw.system.Transaction")
            .unwrap()
            .member("wrap")
            .unwrap();
        assert_eq!(kind, MemberKind::Method);
        assert!(member.shape.starts_with("static wrap("));
        assert!(!member.description.is_empty());
    }

    #[test]
    fn reads_the_require_bindings_of_a_document() {
        let text = "var Site = require('dw/system/Site');\n\
                    const URLUtils = require('dw/web/URLUtils');\n\
                    var helper = require('*/cartridge/scripts/helper');\n";
        let found = bindings(text);
        assert_eq!(
            found.get("Site").map(String::as_str),
            Some("dw.system.Site")
        );
        assert_eq!(
            found.get("URLUtils").map(String::as_str),
            Some("dw.web.URLUtils")
        );
        assert!(!found.contains_key("helper"));
    }

    #[test]
    fn resolves_the_member_under_the_cursor() {
        let text = "var Site = require('dw/system/Site');\nvar id = Site.getCurrent();";
        let line = "var id = Site.getCurrent();";
        let column = line.find("getCurrent").unwrap() + 2;
        assert_eq!(
            member_at(line, column, text),
            Some(("dw.system.Site".into(), "getCurrent".into()))
        );
    }

    #[test]
    fn ignores_a_receiver_that_is_not_an_api_class() {
        let text = "var product = getProduct();";
        let line = "var x = product.custom;";
        let column = line.find("custom").unwrap() + 1;
        assert_eq!(member_at(line, column, text), None);
    }
}
