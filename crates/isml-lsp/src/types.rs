//! The `dw.*` class an expression evaluates to, read from the text: what a variable was
//! assigned, a `@param` or `@type` tag, a global, or what the call or property before it
//! returns. One variable, one type: no branches, no reassignment, no generics.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::api;
use crate::members;
use crate::script::*;
use crate::workspace::Workspace;

/// Objects every script has in scope.
const GLOBALS: [(&str, &str); 4] = [
    ("request", "dw.system.Request"),
    ("session", "dw.system.Session"),
    ("customer", "dw.customer.Customer"),
    ("response", "dw.system.Response"),
];
/// How many steps a type is followed through: `a.b().c` is three.
const MAX_DEPTH: usize = 12;

pub struct Typing<'a> {
    pub file: &'a Path,
    pub text: &'a str,
    pub workspace: &'a Workspace,
    pub open: &'a HashMap<PathBuf, &'a str>,
    /// Comments blanked, so a commented-out assignment types nothing.
    chars: Vec<char>,
    /// Comments kept, for `@param` and `@type`.
    original: Vec<char>,
}

impl<'a> Typing<'a> {
    pub fn new(
        file: &'a Path,
        text: &'a str,
        workspace: &'a Workspace,
        open: &'a HashMap<PathBuf, &'a str>,
    ) -> Self {
        Typing {
            file,
            text,
            workspace,
            open,
            chars: code(file, text),
            original: text.chars().collect(),
        }
    }

    /// `basket.` with the cursor after the dot and part of a member typed: the class and
    /// how many characters are typed.
    pub fn pending_member(&self, offset: usize) -> Option<(String, usize)> {
        let chars = &self.chars;
        let offset = offset.min(chars.len());
        let mut start = offset;
        while start > 0 && is_ident(chars[start - 1]) {
            start -= 1;
        }
        let dot = skip_space_back(chars, start);
        if dot == 0 || chars[dot - 1] != '.' {
            return None;
        }
        let class = self.type_ending_at(dot - 1, 0)?;
        Some((class, offset - start))
    }

    /// `basket.custom.` with part of an attribute typed: the class whose attributes those are,
    /// and how many characters are typed.
    pub fn pending_custom(&self, offset: usize) -> Option<(String, usize)> {
        const MARKER: &str = ".custom.";
        let chars = &self.chars;
        let offset = offset.min(chars.len());
        let mut start = offset;
        while start > 0 && is_ident(chars[start - 1]) {
            start -= 1;
        }
        let marker: Vec<char> = MARKER.chars().collect();
        let receiver_end = start.checked_sub(marker.len())?;
        if chars[receiver_end..start] != marker[..] {
            return None;
        }
        let class = self.type_ending_at(receiver_end, 0)?;
        Some((class, offset - start))
    }

    /// The member under the cursor and the class it belongs to: `basket.getTotalGrossPrice`.
    pub fn member_at(&self, offset: usize) -> Option<(String, String)> {
        let (start, name) = word_at(&self.chars, offset)?;
        let dot = skip_space_back(&self.chars, start);
        if dot == 0 || self.chars[dot - 1] != '.' {
            return None;
        }
        let class = self.type_ending_at(dot - 1, 0)?;
        api::api().member(&class, &name)?;
        Some((class, name))
    }

    /// The class of the expression whose last character is just before `end`.
    pub fn type_ending_at(&self, end: usize, depth: usize) -> Option<String> {
        if depth > MAX_DEPTH {
            return None;
        }
        let chars = &self.chars;
        let end = skip_space_back(chars, end);
        if end == 0 {
            return None;
        }
        if chars[end - 1] == ')' {
            return self.call_type(end - 1, depth);
        }
        let (start, name) = ident_ending_at(chars, end)?;
        let before = skip_space_back(chars, start);
        if before > 0 && chars[before - 1] == '.' {
            let owner = self.type_ending_at(before - 1, depth + 1)?;
            return member_type(&owner, &name);
        }
        self.variable_type(&name, start, depth)
    }

    /// `x.method(...)`, `require('dw/...')` or a cartridge helper's call: what it returns.
    fn call_type(&self, close: usize, depth: usize) -> Option<String> {
        let chars = &self.chars;
        let open = matching_open(chars, close)?;
        let (name_start, name) = ident_ending_at(chars, skip_space_back(chars, open))?;
        if name == "require" {
            let spec: String = chars[open + 1..close]
                .iter()
                .filter(|c| !matches!(c, '\'' | '"' | '`') && !c.is_whitespace())
                .collect();
            let class = spec.strip_prefix("dw/")?.replace('/', ".");
            let qualified = format!("dw.{class}");
            return api::api().class(&qualified).map(|_| qualified);
        }
        let before = skip_space_back(chars, name_start);
        if before == 0 || chars[before - 1] != '.' {
            return None;
        }
        if let Some(owner) = self.type_ending_at(before - 1, depth + 1) {
            return member_type(&owner, &name);
        }
        // A cartridge module's function: what its `@returns` says.
        let definitions = members::at(self.file, self.text, name_start, self.workspace, self.open);
        let returns = definitions
            .iter()
            .find_map(|definition| tagged_type(definition.doc.as_deref()?, "@returns"))?;
        qualify(&returns, None)
    }

    /// A variable's class: its closest declaration before `at`, a `@param` or `@type` tag
    /// naming it, or a global.
    fn variable_type(&self, name: &str, at: usize, depth: usize) -> Option<String> {
        if let Some(declared) = self.declaration_before(name, at) {
            if let Some(tagged) = doc_type_above(&self.original, declared, "@type") {
                return qualify(&tagged, None);
            }
            let equals = skip_space(&self.chars, declared + name.chars().count());
            if is_assignment(&self.chars, equals) {
                let value_end = statement_end(&self.chars, equals + 1);
                if let Some(class) = self.type_ending_at(value_end, depth + 1) {
                    return Some(class);
                }
            }
        }
        if let Some(tagged) = self.param_tag_before(name, at) {
            return qualify(&tagged, None);
        }
        GLOBALS
            .iter()
            .find(|(global, _)| *global == name)
            .map(|(_, class)| class.to_string())
    }

    /// The last `var name`, `let name` or `const name` before `at`.
    fn declaration_before(&self, name: &str, at: usize) -> Option<usize> {
        words(&self.chars, name)
            .into_iter()
            .filter(|found| *found < at)
            .rfind(|found| {
                let before = skip_space_back(&self.chars, *found);
                matches!(
                    ident_ending_at(&self.chars, before)
                        .map(|(_, word)| word)
                        .as_deref(),
                    Some("var" | "let" | "const")
                )
            })
    }

    /// `@param {dw.order.Basket} basket` in the last comment before `at` that documents it.
    fn param_tag_before(&self, name: &str, at: usize) -> Option<String> {
        let original = &self.original;
        let head: String = original[..at.min(original.len())].iter().collect();
        head.rmatch_indices("@param").find_map(|(index, _)| {
            let rest = &head[index + "@param".len()..];
            let line = rest.lines().next().unwrap_or("");
            let (kind, after) = braced(line)?;
            let tagged = after
                .split_whitespace()
                .next()?
                .trim_start_matches('[')
                .trim_end_matches(']');
            let tagged = tagged.split('=').next().unwrap_or(tagged);
            (tagged == name).then(|| kind.to_string())
        })
    }
}

/// The class a member of `owner` gives: a method's return type, a property's type.
fn member_type(owner: &str, name: &str) -> Option<String> {
    let (member, kind) = api::api().member(owner, name)?;
    let shape = member.shape.as_str();
    let declared = match kind {
        api::MemberKind::Method => shape.rsplit_once(") :")?.1,
        _ => shape.split(" (").next()?,
    };
    qualify(declared.trim(), Some(owner))
}

/// `Basket` as `dw.order.Basket`. A short name that several packages define is taken
/// from `near`'s package, or not at all.
pub fn qualify(name: &str, near: Option<&str>) -> Option<String> {
    let name = name
        .trim()
        .trim_start_matches('{')
        .trim_end_matches('}')
        .trim();
    let name = name.split(['<', '|', ' ']).next()?;
    if api::api().class(name).is_some() {
        return Some(name.to_string());
    }
    let candidates = api::api().by_short_name(name);
    match candidates.as_slice() {
        [only] => Some(only.to_string()),
        [] => None,
        several => {
            let package = near?.rsplit_once('.')?.0;
            several
                .iter()
                .find(|candidate| candidate.rsplit_once('.').map(|(p, _)| p) == Some(package))
                .map(|candidate| candidate.to_string())
        }
    }
}

/// `{Type}` at the start of `text`, and what follows it.
fn braced(text: &str) -> Option<(&str, &str)> {
    let open = text.trim_start().strip_prefix('{')?;
    let (kind, rest) = open.split_once('}')?;
    Some((kind.trim(), rest))
}

/// The type of the first `tag` in a documentation block: `@returns {dw.order.Basket} ...`.
fn tagged_type(doc: &str, tag: &str) -> Option<String> {
    doc.lines().find_map(|line| {
        let rest = line.trim().strip_prefix(tag)?;
        braced(rest).map(|(kind, _)| kind.to_string())
    })
}

/// `/** @type {dw.catalog.Product} */` on the lines just above a declaration.
fn doc_type_above(original: &[char], at: usize, tag: &str) -> Option<String> {
    let line_start = original[..at.min(original.len())]
        .iter()
        .rposition(|c| *c == '\n')
        .map_or(0, |index| index + 1);
    let end = skip_space_back(original, line_start);
    if end < 2 || original[end - 2..end] != ['*', '/'] {
        return None;
    }
    let opening: Vec<char> = "/**".chars().collect();
    let start = (0..end.saturating_sub(2))
        .rev()
        .find(|index| original[*index..].starts_with(&opening))?;
    let body: String = original[start + 3..end - 2].iter().collect();
    let cleaned: String = body
        .lines()
        .map(|line| line.trim().trim_start_matches('*').trim())
        .collect::<Vec<_>>()
        .join("\n");
    tagged_type(&cleaned, tag)
}

/// The end of the value assigned at `from`: the `;` or the line break that ends it, unless
/// the next line goes on with `.`.
fn statement_end(chars: &[char], from: usize) -> usize {
    let mut depth = 0usize;
    let mut index = from;
    while index < chars.len() {
        match chars[index] {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' if depth == 0 => return index,
            ')' | ']' | '}' => depth -= 1,
            ';' | ',' if depth == 0 => return index,
            '\n' if depth == 0 => {
                let next = skip_space(chars, index);
                if chars.get(next) != Some(&'.') {
                    return index;
                }
            }
            '\'' | '"' | '`' => {
                let quote = chars[index];
                index += 1;
                while index < chars.len() && chars[index] != quote {
                    if chars[index] == '\\' {
                        index += 1;
                    }
                    index += 1;
                }
            }
            _ => {}
        }
        index += 1;
    }
    chars.len()
}

/// The `(` that `close` closes, scanning back.
fn matching_open(chars: &[char], close: usize) -> Option<usize> {
    let mut depth = 0usize;
    for index in (0..close).rev() {
        match chars[index] {
            ')' | ']' | '}' => depth += 1,
            '(' | '[' | '{' if depth == 0 => {
                return (chars[index] == '(').then_some(index);
            }
            '(' | '[' | '{' => depth -= 1,
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    const MANAGERS: &str =
        "var BasketMgr = require('dw/order/BasketMgr');\nvar Site = require('dw/system/Site');\n";

    /// The class completed at the end of `text`.
    fn completed(text: &str) -> Option<String> {
        let workspace = Workspace::default();
        let open = HashMap::new();
        let typing = Typing::new(Path::new("x.js"), text, &workspace, &open);
        typing
            .pending_member(text.chars().count())
            .map(|(class, _)| class)
    }

    #[test]
    fn follows_the_return_type_of_a_call() {
        let text = format!("{MANAGERS}Site.getCurrent().");
        assert_eq!(completed(&text).as_deref(), Some("dw.system.Site"));
    }

    #[test]
    fn types_a_variable_from_what_it_was_assigned() {
        let text = format!("{MANAGERS}var basket = BasketMgr.getCurrentBasket();\nbasket.");
        assert_eq!(completed(&text).as_deref(), Some("dw.order.Basket"));
    }

    #[test]
    fn follows_inherited_members_and_properties() {
        let text = format!(
            "{MANAGERS}var basket = BasketMgr.getCurrentBasket();\nbasket.getDefaultShipment()."
        );
        assert_eq!(completed(&text).as_deref(), Some("dw.order.Shipment"));
        let text = format!(
            "{MANAGERS}var shipment = BasketMgr.getCurrentBasket().defaultShipment;\nshipment."
        );
        assert_eq!(completed(&text).as_deref(), Some("dw.order.Shipment"));
    }

    #[test]
    fn reads_a_param_tag_and_a_type_tag() {
        let text = "/**\n * @param {dw.order.Basket} cart - the cart\n */\nfunction total(cart) {\n    cart.";
        assert_eq!(completed(text).as_deref(), Some("dw.order.Basket"));
        let text = "/** @type {dw.catalog.Product} */\nvar thing = lookUp();\nthing.";
        assert_eq!(completed(text).as_deref(), Some("dw.catalog.Product"));
    }

    #[test]
    fn knows_the_globals() {
        assert_eq!(completed("request.").as_deref(), Some("dw.system.Request"));
        assert_eq!(
            completed("session.getCustomer().").as_deref(),
            Some("dw.customer.Customer")
        );
    }

    #[test]
    fn says_nothing_about_what_it_cannot_type() {
        assert_eq!(completed("unknown."), None);
        assert_eq!(completed("var x = compute();\nx."), None);
        // A commented-out assignment types nothing.
        assert_eq!(
            completed("// var basket = require('dw/order/BasketMgr').getCurrentBasket();\nbasket."),
            None
        );
    }

    #[test]
    fn finds_the_member_under_the_cursor_and_the_custom_receiver() {
        let text = format!(
            "{MANAGERS}var cart = BasketMgr.getCurrentBasket();\ncart.getTotalGrossPrice();"
        );
        let workspace = Workspace::default();
        let open = HashMap::new();
        let typing = Typing::new(Path::new("x.js"), &text, &workspace, &open);
        let offset = text.find("getTotalGrossPrice").unwrap();
        assert_eq!(
            typing.member_at(offset),
            Some((
                "dw.order.Basket".to_string(),
                "getTotalGrossPrice".to_string()
            ))
        );

        let text = format!("{MANAGERS}var cart = BasketMgr.getCurrentBasket();\ncart.custom.gi");
        let typing = Typing::new(Path::new("x.js"), &text, &workspace, &open);
        assert_eq!(
            typing.pending_custom(text.chars().count()),
            Some(("dw.order.Basket".to_string(), 2))
        );
    }

    #[test]
    fn qualifies_a_short_name_by_the_package_it_comes_from() {
        assert_eq!(qualify("Basket", None).as_deref(), Some("dw.order.Basket"));
        assert_eq!(
            qualify("Iterator", Some("dw.util.Collection")).as_deref(),
            Some("dw.util.Iterator")
        );
        assert_eq!(qualify("Iterator", None), None);
        assert_eq!(qualify("NoSuchClass", None), None);
    }

    #[test]
    fn reads_what_a_cartridge_function_returns() {
        let root = std::env::temp_dir().join("isml-lsp-types-returns");
        let _ = fs::remove_dir_all(&root);
        let helper = root.join("cartridges/app_brand/cartridge/scripts/cartHelpers.js");
        fs::create_dir_all(helper.parent().unwrap()).unwrap();
        fs::write(
            &helper,
            "/**\n * @returns {dw.order.Basket} the basket\n */\nfunction current() {}\nmodule.exports = { current: current };\n",
        )
        .unwrap();
        let workspace = Workspace::scan(std::slice::from_ref(&root), &serde_json::Value::Null);
        let caller = root.join("cartridges/app_brand/cartridge/controllers/Cart.js");
        let text = "var helpers = require('*/cartridge/scripts/cartHelpers');\nvar cart = helpers.current();\ncart.";
        let open = HashMap::new();
        let typing = Typing::new(&caller, text, &workspace, &open);
        assert_eq!(
            typing
                .pending_member(text.chars().count())
                .map(|(class, _)| class)
                .as_deref(),
            Some("dw.order.Basket")
        );
    }
}
