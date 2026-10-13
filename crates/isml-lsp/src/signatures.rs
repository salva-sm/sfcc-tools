//! Signature help on a call: the cartridge function it reaches along the cartridge path, or
//! the `dw.*` method of a class the file requires.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use lsp_types::{
    Documentation, MarkupContent, MarkupKind, ParameterInformation, ParameterLabel, SignatureHelp,
    SignatureInformation,
};
use tree_sitter::Node;

use crate::api;
use crate::js::{Parsed, Script};
use crate::members::{self, Definition};
use crate::workspace::Workspace;

/// The call the cursor is inside: the callee's name, and which argument the cursor is on.
struct Call<'a> {
    name: Node<'a>,
    active: u32,
}

/// `offset` is in characters, as the rest of the server counts.
pub fn help(
    file: &Path,
    text: &str,
    offset: usize,
    workspace: &Workspace,
    open: &HashMap<PathBuf, &str>,
) -> Option<SignatureHelp> {
    let parsed = Parsed::new(file, text)?;
    let script = parsed.script();
    let at = text
        .char_indices()
        .nth(offset)
        .map_or(text.len(), |(at, _)| at);
    let call = call_at(&script, at)?;
    let definitions = members::of(&script, call.name, file, workspace, open);
    let signatures: Vec<SignatureInformation> = if definitions.is_empty() {
        api_signature(text, call.name).into_iter().collect()
    } else {
        definitions.iter().filter_map(cartridge_signature).collect()
    };
    (!signatures.is_empty()).then_some(SignatureHelp {
        signatures,
        active_signature: Some(0),
        active_parameter: Some(call.active),
    })
}

/// The innermost call whose parentheses hold `at`, and the commas between its arguments
/// before it: commas inside strings, arrays or nested calls belong to those.
fn call_at<'a>(script: &Script<'a>, at: usize) -> Option<Call<'a>> {
    let mut node = script.node_at(at)?;
    loop {
        if node.kind() == "ERROR" {
            if let Some(call) = unclosed_call(node, at) {
                return Some(call);
            }
        }
        if node.kind() == "arguments" && node.start_byte() < at {
            // `f(a)|` is past the call; `f(a|` with the `)` not typed yet is inside it.
            let closed = script.text(node).ends_with(')') && !node.has_error();
            if at < node.end_byte() || !closed {
                let call = script
                    .parent(node)
                    .filter(|parent| parent.kind() == "call_expression")?;
                let function = call.child_by_field_name("function")?;
                let name = match function.kind() {
                    "member_expression" => function.child_by_field_name("property")?,
                    "identifier" => function,
                    _ => return None,
                };
                let mut cursor = node.walk();
                let active = node
                    .children(&mut cursor)
                    .filter(|child| child.kind() == "," && child.start_byte() < at)
                    .count() as u32;
                return Some(Call { name, active });
            }
        }
        node = script.parent(node)?;
    }
}

/// A call still being typed at the end of the file parses as an error holding the callee,
/// a bare `(`, and the arguments and commas so far; what was closed inside it (a nested
/// call, an array, a string) is a node of its own. The last `(` after a name is the call;
/// an unclosed `[` or `{` after it means the cursor is in there instead.
fn unclosed_call(error: Node, at: usize) -> Option<Call> {
    let mut cursor = error.walk();
    let children: Vec<Node> = error
        .children(&mut cursor)
        .filter(|child| child.start_byte() < at)
        .collect();
    let open = children.iter().enumerate().rposition(|(index, child)| {
        child.kind() == "("
            && index > 0
            && matches!(
                children[index - 1].kind(),
                "identifier" | "member_expression"
            )
    })?;
    let after = &children[open + 1..];
    if after.iter().any(|child| matches!(child.kind(), "[" | "{")) {
        return None;
    }
    let callee = children[open - 1];
    let name = match callee.kind() {
        "member_expression" => callee.child_by_field_name("property")?,
        _ => callee,
    };
    let active = after.iter().filter(|child| child.kind() == ",").count() as u32;
    Some(Call { name, active })
}

fn cartridge_signature(definition: &Definition) -> Option<SignatureInformation> {
    let label = definition.signature.clone()?;
    let described = definition
        .doc
        .as_deref()
        .map(described_params)
        .unwrap_or_default();
    // The label is `name(` and the parameters joined by `, `, so each range is known.
    let mut from = label.find('(')? + 1;
    let mut parameters = Vec::new();
    for (written, name) in &definition.parameters {
        let to = from + written.len();
        parameters.push(ParameterInformation {
            label: ParameterLabel::LabelOffsets([utf16(&label[..from]), utf16(&label[..to])]),
            documentation: name
                .as_ref()
                .and_then(|name| described.get(name))
                .cloned()
                .map(markdown),
        });
        from = to + ", ".len();
    }
    Some(SignatureInformation {
        label,
        documentation: definition
            .doc
            .as_deref()
            .map(description)
            .filter(|text| !text.is_empty())
            .map(markdown),
        parameters: Some(parameters),
        active_parameter: None,
    })
}

/// `PriceBookMgr.getApplicablePriceBooks(`: the method's shape from the platform reference.
fn api_signature(text: &str, name: Node) -> Option<SignatureInformation> {
    let start = name.start_byte();
    let line_start = text[..start].rfind('\n').map_or(0, |index| index + 1);
    let line = text[line_start..].lines().next().unwrap_or("");
    let column = text[line_start..start].chars().count();
    let (class, name) = api::member_at(line, column, text)?;
    let (member, kind) = api::api().class(&class)?.member(&name)?;
    if kind != api::MemberKind::Method {
        return None;
    }
    let label = member.shape.clone();
    let parameters = api_parameters(&label)
        .into_iter()
        .map(|range| ParameterInformation {
            label: ParameterLabel::LabelOffsets(range),
            documentation: None,
        })
        .collect();
    Some(SignatureInformation {
        label,
        documentation: (!member.description.is_empty())
            .then(|| markdown(member.description.clone())),
        parameters: Some(parameters),
        active_parameter: None,
    })
}

/// The UTF-16 range of each parameter between the first `(` of a reference shape and its
/// matching `)`: `assign(priceBook : PriceBook, siteId : String) : boolean`.
fn api_parameters(label: &str) -> Vec<[u32; 2]> {
    let Some(open) = label.find('(') else {
        return Vec::new();
    };
    let mut found = Vec::new();
    let mut depth = 0usize;
    let mut start = open + 1;
    for (index, c) in label[open..].char_indices().map(|(at, c)| (open + at, c)) {
        match c {
            '(' | '[' | '<' | '{' => depth += 1,
            ')' | ']' | '>' | '}' if depth > 1 => depth -= 1,
            ',' | ')' if depth == 1 => {
                let segment = &label[start..index];
                let trimmed = segment.trim();
                if !trimmed.is_empty() {
                    let from = start + (segment.len() - segment.trim_start().len());
                    found.push([utf16(&label[..from]), utf16(&label[..from + trimmed.len()])]);
                }
                if c == ')' {
                    break;
                }
                start = index + 1;
            }
            _ => {}
        }
    }
    found
}

fn utf16(text: &str) -> u32 {
    text.encode_utf16().count() as u32
}

/// What each `@param` says, by parameter name: `{Type} description`.
fn described_params(doc: &str) -> HashMap<String, String> {
    let mut described = HashMap::new();
    for line in doc.lines() {
        let Some(rest) = line.trim().strip_prefix("@param") else {
            continue;
        };
        let rest = rest.trim();
        let (kind, rest) = match rest.strip_prefix('{') {
            Some(after) => match after.split_once('}') {
                Some((kind, rest)) => (Some(kind), rest.trim()),
                None => (None, rest),
            },
            None => (None, rest),
        };
        let (name, text) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
        let name = name
            .trim_start_matches('[')
            .trim_end_matches(']')
            .split('=')
            .next()
            .unwrap_or(name);
        let text = text.trim().trim_start_matches('-').trim();
        let entry = match kind {
            Some(kind) => format!("`{kind}` {text}"),
            None => text.to_string(),
        };
        described.insert(name.to_string(), entry.trim().to_string());
    }
    described
}

/// The documentation before its first tag.
fn description(doc: &str) -> String {
    doc.lines()
        .take_while(|line| !line.trim_start().starts_with('@'))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

fn markdown(value: String) -> Documentation {
    Documentation::MarkupContent(MarkupContent {
        kind: MarkupKind::Markdown,
        value,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The callee and the active argument with the cursor at the end of `text`.
    fn call(text: &str) -> Option<(String, u32)> {
        let parsed = Parsed::new(Path::new("x.js"), text).unwrap();
        let script = parsed.script();
        let call = call_at(&script, text.len())?;
        Some((script.text(call.name).to_string(), call.active))
    }

    #[test]
    fn finds_the_call_and_the_argument_the_cursor_is_on() {
        assert_eq!(call("helpers.total("), Some(("total".into(), 0)));
        assert_eq!(
            call("helpers.total(a, [1, 2], f(x, y), "),
            Some(("total".into(), 3))
        );
        assert_eq!(call("helpers.total(a, f(x, "), Some(("f".into(), 1)));
        assert_eq!(call("helpers.total(a);\nb"), None);
        assert_eq!(call("var x = { a: 1, "), None);
        // Still being typed, with code after it.
        let text = "x = 1;
helpers.total(a, ";
        let parsed = Parsed::new(
            Path::new("x.js"),
            &format!(
                "{text}
var y = 2;"
            ),
        )
        .unwrap();
        let script = parsed.script();
        let found = call_at(&script, text.len()).unwrap();
        assert_eq!((script.text(found.name), found.active), ("total", 1));
    }

    #[test]
    fn a_comma_in_a_string_or_a_template_is_not_an_argument() {
        assert_eq!(call("helpers.total('a,b', "), Some(("total".into(), 1)));
        assert_eq!(
            call("helpers.total(`a,${b},c`, "),
            Some(("total".into(), 1))
        );
        assert_eq!(call("helpers.total(')', "), Some(("total".into(), 1)));
    }

    #[test]
    fn labels_each_parameter_of_a_cartridge_function_whole() {
        let definition = Definition {
            storefronts: Vec::new(),
            cartridge: "app_brand".into(),
            path: PathBuf::from("cartHelpers.js"),
            line: 0,
            start: 0,
            end: 5,
            signature: Some("total(basket, b = [1, 2], { c, d })".into()),
            parameters: vec![
                ("basket".into(), Some("basket".into())),
                ("b = [1, 2]".into(), Some("b".into())),
                ("{ c, d }".into(), None),
            ],
            doc: Some("Adds.\n@param {dw.order.Basket} basket - the basket".into()),
        };
        let signature = cartridge_signature(&definition).unwrap();
        let labels: Vec<&str> = signature
            .parameters
            .unwrap()
            .iter()
            .map(|parameter| match parameter.label {
                ParameterLabel::LabelOffsets([from, to]) => {
                    &definition.signature.as_deref().unwrap()[from as usize..to as usize]
                }
                ParameterLabel::Simple(_) => "",
            })
            .collect();
        assert_eq!(labels, ["basket", "b = [1, 2]", "{ c, d }"]);
    }

    #[test]
    fn splits_the_parameters_of_a_reference_shape() {
        let label = "static assign(priceBook : PriceBook, siteId : String) : boolean";
        assert_eq!(api_parameters(label), [[14, 35], [37, 52]]);
        assert!(api_parameters("total()").is_empty());
    }

    #[test]
    fn reads_what_each_param_tag_says() {
        let doc = "Adds.\n@param {dw.order.Basket} basket - the basket\n@param {Object} [options] - extras\n@returns {number} total";
        let described = described_params(doc);
        assert_eq!(described["basket"], "`dw.order.Basket` the basket");
        assert_eq!(described["options"], "`Object` extras");
        assert_eq!(description(doc), "Adds.");
    }

    #[test]
    fn helps_with_a_method_of_the_platform_api() {
        let text = "var PriceBookMgr = require('dw/catalog/PriceBookMgr');\nPriceBookMgr.assignPriceBookToSite(book, ";
        let help = help(
            Path::new("Cart.js"),
            text,
            text.chars().count(),
            &Workspace::default(),
            &HashMap::new(),
        )
        .unwrap();
        assert!(help.signatures[0]
            .label
            .contains("assignPriceBookToSite(priceBook"));
        assert_eq!(help.active_parameter, Some(1));
    }
}
