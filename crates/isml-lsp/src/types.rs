//! The `dw.*` class an expression evaluates to: what a variable was declared with, a
//! `@param` or `@type` tag, a global, or what the call or property before it returns.
//! Read from the parsed script (see `js`), so a name is the one its own function declares.
//! One variable, one type, the value it is declared with: no branches, no reassignment, no
//! generics.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tree_sitter::Node;

use crate::api;
use crate::js::{self, Origin, Parsed, Script};
use crate::members;
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

/// Where a script is, for the cartridge functions whose `@returns` it follows.
pub struct Types<'s, 'a> {
    pub script: &'s Script<'a>,
    pub file: &'s Path,
    pub workspace: &'s Workspace,
    pub open: &'s HashMap<PathBuf, &'s str>,
}

impl<'a> Types<'_, 'a> {
    /// The class of the expression `node`.
    pub fn of(&self, node: Node<'a>) -> Option<String> {
        self.within(node, 0)
    }

    fn within(&self, node: Node<'a>, depth: usize) -> Option<String> {
        if depth > MAX_DEPTH {
            return None;
        }
        match node.kind() {
            "parenthesized_expression" => self.within(node.named_child(0)?, depth + 1),
            "call_expression" => self.call_type(node, depth),
            "member_expression" => {
                let owner = self.within(node.child_by_field_name("object")?, depth + 1)?;
                member_type(
                    &owner,
                    self.script.text(node.child_by_field_name("property")?),
                )
            }
            "identifier" => self.variable_type(node, depth),
            _ => None,
        }
    }

    /// `require('dw/...')`, `x.method(...)` or a cartridge function's call: what it returns.
    fn call_type(&self, call: Node<'a>, depth: usize) -> Option<String> {
        if let Some(Origin::Require(spec)) = self.script.module(call) {
            let qualified = format!("dw.{}", spec.strip_prefix("dw/")?.replace('/', "."));
            return api::api().class(&qualified).map(|_| qualified);
        }
        let function = call.child_by_field_name("function")?;
        let name = match function.kind() {
            "member_expression" => {
                let property = function.child_by_field_name("property")?;
                let object = function.child_by_field_name("object")?;
                if let Some(owner) = self.within(object, depth + 1) {
                    return member_type(&owner, self.script.text(property));
                }
                property
            }
            "identifier" => function,
            _ => return None,
        };
        // A cartridge module's function: what the `@returns` of the copy that runs says.
        let definitions = members::of(self.script, name, self.file, self.workspace, self.open);
        let returns = definitions
            .iter()
            .find_map(|definition| tagged_type(definition.doc.as_deref()?, "@returns"))?;
        qualify(&returns, None)
    }

    /// A variable's class: a `@type` tag on its declaration, the value it is declared
    /// with, or the `@param` tag of the function it is a parameter of. A name nothing in
    /// the file declares may be a global; one that is declared but cannot be typed is not.
    fn variable_type(&self, node: Node<'a>, depth: usize) -> Option<String> {
        let Some(declaration) = self.script.declaration_of(node) else {
            let name = self.script.text(node);
            return GLOBALS
                .iter()
                .find(|(global, _)| *global == name)
                .map(|(_, class)| class.to_string());
        };
        if let Some(tagged) = self
            .script
            .doc_of(declaration)
            .and_then(|doc| tagged_type(&doc, "@type"))
        {
            return qualify(&tagged, None);
        }
        let parent = self.script.parent(declaration)?;
        if parent.kind() == "variable_declarator" {
            let value = parent.child_by_field_name("value")?;
            return self.within(value, depth + 1);
        }
        let function = self.parameter_of(declaration)?;
        let doc = self.script.doc_of(function)?;
        qualify(&param_type(&doc, self.script.text(declaration))?, None)
    }

    /// The function `declaration` is a parameter of, when it is one.
    fn parameter_of(&self, declaration: Node<'a>) -> Option<Node<'a>> {
        let mut current = self.script.parent(declaration)?;
        loop {
            if matches!(
                current.kind(),
                "function_declaration"
                    | "generator_function_declaration"
                    | "function_expression"
                    | "generator_function"
                    | "arrow_function"
                    | "method_definition"
            ) {
                let parameters = current
                    .child_by_field_name("parameters")
                    .or_else(|| current.child_by_field_name("parameter"))?;
                let inside = parameters.start_byte() <= declaration.start_byte()
                    && declaration.end_byte() <= parameters.end_byte();
                return inside.then_some(current);
            }
            current = self.script.parent(current)?;
        }
    }
}

/// `basket.` with the cursor after the dot and part of a member typed: the class, and how
/// many characters are typed. `offset` is in characters.
pub fn pending_member(
    file: &Path,
    text: &str,
    offset: usize,
    workspace: &Workspace,
    open: &HashMap<PathBuf, &str>,
) -> Option<(String, usize)> {
    let pending = js::Pending::at(text, offset)?;
    let parsed = Parsed::new(file, &pending.text)?;
    let script = parsed.script();
    let object = pending.receiver(&script)?;
    let types = Types {
        script: &script,
        file,
        workspace,
        open,
    };
    Some((types.of(object)?, pending.typed))
}

/// `basket.custom.` with part of an attribute typed: the class whose attributes those are,
/// and how many characters are typed.
pub fn pending_custom(
    file: &Path,
    text: &str,
    offset: usize,
    workspace: &Workspace,
    open: &HashMap<PathBuf, &str>,
) -> Option<(String, usize)> {
    let pending = js::Pending::at(text, offset)?;
    let parsed = Parsed::new(file, &pending.text)?;
    let script = parsed.script();
    let custom = pending.receiver(&script)?;
    if custom.kind() != "member_expression"
        || script.text(custom.child_by_field_name("property")?) != "custom"
    {
        return None;
    }
    let types = Types {
        script: &script,
        file,
        workspace,
        open,
    };
    Some((
        types.of(custom.child_by_field_name("object")?)?,
        pending.typed,
    ))
}

/// The member under the cursor and the class it belongs to: `basket.getTotalGrossPrice`.
pub fn member_at(
    file: &Path,
    text: &str,
    offset: usize,
    workspace: &Workspace,
    open: &HashMap<PathBuf, &str>,
) -> Option<(String, String)> {
    let parsed = Parsed::new(file, text)?;
    let script = parsed.script();
    let at = text
        .char_indices()
        .nth(offset)
        .map_or(text.len(), |(at, _)| at);
    let types = Types {
        script: &script,
        file,
        workspace,
        open,
    };
    types.member_named(script.name_at(at)?)
}

impl<'a> Types<'_, 'a> {
    /// The class `property` is a member of, and its name, when the platform reference has it.
    pub fn member_named(&self, property: Node<'a>) -> Option<(String, String)> {
        let member = self
            .script
            .parent(property)
            .filter(|parent| parent.kind() == "member_expression")?;
        let class = self.of(member.child_by_field_name("object")?)?;
        let name = self.script.text(property);
        api::api().member(&class, name)?;
        Some((class, name.to_string()))
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

/// The type `@param {dw.order.Basket} name` gives `name` in a documentation block.
fn param_type(doc: &str, name: &str) -> Option<String> {
    doc.lines().find_map(|line| {
        let rest = line.trim().strip_prefix("@param")?;
        let (kind, after) = braced(rest)?;
        let tagged = after
            .split_whitespace()
            .next()?
            .trim_start_matches('[')
            .trim_end_matches(']');
        let tagged = tagged.split('=').next().unwrap_or(tagged);
        (tagged == name).then(|| kind.to_string())
    })
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
        pending_member(
            Path::new("x.js"),
            text,
            text.chars().count(),
            &workspace,
            &HashMap::new(),
        )
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
    fn a_name_is_the_one_its_own_function_declares() {
        // `basket` in `b` is `b`'s parameter, whatever `a` declared under that name.
        let text = format!(
            "{MANAGERS}function a() {{ var basket = BasketMgr.getCurrentBasket(); }}\n/**\n * @param {{dw.order.Order}} basket\n */\nfunction b(basket) {{\n    basket."
        );
        assert_eq!(completed(&text).as_deref(), Some("dw.order.Order"));
        // A `@param` of another function says nothing about this one's `x`.
        let text = "/**\n * @param {dw.order.Order} x\n */\nfunction a(x) {}\nfunction b() {\n    var x = unknown();\n    x.";
        assert_eq!(completed(text), None);
    }

    #[test]
    fn a_local_variable_is_not_the_global_of_the_same_name() {
        assert_eq!(completed("var request = build();\nrequest."), None);
        assert_eq!(
            completed("function f(session) {\n    session."),
            None,
            "an untagged parameter"
        );
    }

    #[test]
    fn a_plain_comment_does_not_borrow_an_earlier_type_tag() {
        let text = "/** @type {dw.catalog.Product} */\nvar first = lookUp();\n/* not a tag */\nvar second = lookUp();\nsecond.";
        assert_eq!(completed(text), None);
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
        let offset = text.find("getTotalGrossPrice").unwrap();
        assert_eq!(
            member_at(Path::new("x.js"), &text, offset, &workspace, &open),
            Some((
                "dw.order.Basket".to_string(),
                "getTotalGrossPrice".to_string()
            ))
        );

        let text = format!("{MANAGERS}var cart = BasketMgr.getCurrentBasket();\ncart.custom.gi");
        assert_eq!(
            pending_custom(
                Path::new("x.js"),
                &text,
                text.chars().count(),
                &workspace,
                &open
            ),
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
        let open = HashMap::new();
        for text in [
            "var helpers = require('*/cartridge/scripts/cartHelpers');\nvar cart = helpers.current();\ncart.",
            "var { current } = require('*/cartridge/scripts/cartHelpers');\nvar cart = current();\ncart.",
        ] {
            assert_eq!(
                pending_member(&caller, text, text.chars().count(), &workspace, &open)
                    .map(|(class, _)| class)
                    .as_deref(),
                Some("dw.order.Basket"),
                "{text}"
            );
        }
    }
}
