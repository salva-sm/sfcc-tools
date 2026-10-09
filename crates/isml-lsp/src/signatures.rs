//! Signature help on a call: the cartridge function it reaches along the cartridge path, or
//! the `dw.*` method of a class the file requires.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use lsp_types::{
    Documentation, MarkupContent, MarkupKind, ParameterInformation, ParameterLabel, SignatureHelp,
    SignatureInformation,
};

use crate::api;
use crate::members::{self, Definition};
use crate::script::{code, ident_ending_at, skip_space_back};
use crate::types::Typing;
use crate::workspace::Workspace;

/// How far back an unclosed `(` is looked for: a call spans a few lines, not a file.
const MAX_SCAN: usize = 4000;

/// The call the cursor is inside: where the callee's name starts, and which argument the
/// cursor is on.
#[derive(Debug, PartialEq, Eq)]
struct Call {
    name_start: usize,
    active: u32,
}

pub fn help(
    file: &Path,
    text: &str,
    offset: usize,
    workspace: &Workspace,
    open: &HashMap<PathBuf, &str>,
) -> Option<SignatureHelp> {
    let chars = code(file, text);
    let call = call_at(&chars, offset)?;
    let definitions = members::at(file, text, call.name_start, workspace, open);
    let signatures: Vec<SignatureInformation> = if definitions.is_empty() {
        let typing = Typing::new(file, text, workspace, open);
        typing
            .member_at(call.name_start)
            .and_then(|(class, name)| api_signature(&class, &name))
            .into_iter()
            .collect()
    } else {
        definitions.iter().filter_map(cartridge_signature).collect()
    };
    (!signatures.is_empty()).then_some(SignatureHelp {
        signatures,
        active_signature: Some(0),
        active_parameter: Some(call.active),
    })
}

/// The innermost unclosed `(` before `offset` that follows a name, and the commas between.
fn call_at(chars: &[char], offset: usize) -> Option<Call> {
    let offset = offset.min(chars.len());
    let mut depth = 0usize;
    let mut commas = 0u32;
    for index in (offset.saturating_sub(MAX_SCAN)..offset).rev() {
        match chars[index] {
            ')' | ']' | '}' => depth += 1,
            '[' | '{' if depth == 0 => return None,
            '[' | '{' => depth -= 1,
            '(' if depth == 0 => {
                let (name_start, _) = ident_ending_at(chars, skip_space_back(chars, index))?;
                return Some(Call {
                    name_start,
                    active: commas,
                });
            }
            '(' => depth -= 1,
            ',' if depth == 0 => commas += 1,
            ';' if depth == 0 => return None,
            _ => {}
        }
    }
    None
}

fn cartridge_signature(definition: &Definition) -> Option<SignatureInformation> {
    let label = definition.signature.clone()?;
    let described = definition
        .doc
        .as_deref()
        .map(described_params)
        .unwrap_or_default();
    let parameters = parameters(&label)
        .into_iter()
        .map(|(range, name)| ParameterInformation {
            label: ParameterLabel::LabelOffsets(range),
            documentation: described.get(&name).cloned().map(markdown),
        })
        .collect();
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

/// `basket.getProductLineItems(`: the method's shape from the platform reference.
fn api_signature(class: &str, name: &str) -> Option<SignatureInformation> {
    let (member, kind) = api::api().member(class, name)?;
    if kind != api::MemberKind::Method {
        return None;
    }
    let label = member.shape.clone();
    let parameters = parameters(&label)
        .into_iter()
        .map(|(range, _)| ParameterInformation {
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

/// Each parameter between the label's parentheses: its UTF-16 range in the label, and its
/// name (`basket` of `basket : Basket` or `basket = null`).
fn parameters(label: &str) -> Vec<([u32; 2], String)> {
    let chars: Vec<char> = label.chars().collect();
    let Some(open) = chars.iter().position(|c| *c == '(') else {
        return Vec::new();
    };
    let Some(close) = chars.iter().rposition(|c| *c == ')') else {
        return Vec::new();
    };
    let utf16 = |index: usize| -> u32 { chars[..index].iter().map(|c| c.len_utf16() as u32).sum() };
    let mut found = Vec::new();
    let mut start = open + 1;
    for index in open + 1..=close {
        if index != close && chars[index] != ',' {
            continue;
        }
        let segment: String = chars[start..index].iter().collect();
        let leading = segment.len() - segment.trim_start().len();
        let trimmed = segment.trim();
        if !trimmed.is_empty() {
            let from = start + segment[..leading].chars().count();
            let to = from + trimmed.chars().count();
            let name = trimmed
                .split([':', '=', ' '])
                .next()
                .unwrap_or(trimmed)
                .to_string();
            found.push(([utf16(from), utf16(to)], name));
        }
        start = index + 1;
    }
    found
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

    fn call(text: &str) -> Option<Call> {
        let chars: Vec<char> = text.chars().collect();
        call_at(&chars, chars.len())
    }

    #[test]
    fn finds_the_call_and_the_argument_the_cursor_is_on() {
        assert_eq!(
            call("helpers.total("),
            Some(Call {
                name_start: 8,
                active: 0
            })
        );
        assert_eq!(
            call("helpers.total(a, [1, 2], f(x, y), "),
            Some(Call {
                name_start: 8,
                active: 3
            })
        );
        assert_eq!(
            call("helpers.total(a, f(x, "),
            Some(Call {
                name_start: 17,
                active: 1
            })
        );
        assert_eq!(call("helpers.total(a);\nb"), None);
        assert_eq!(call("var x = { a: 1, "), None);
    }

    #[test]
    fn splits_the_parameters_of_a_label() {
        let found = parameters("static assign(priceBook : PriceBook, siteId : String) : boolean");
        assert_eq!(
            found,
            [
                ([14, 35], "priceBook".to_string()),
                ([37, 52], "siteId".to_string())
            ]
        );
        assert!(parameters("total()").is_empty());
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

    #[test]
    fn helps_with_a_method_of_a_typed_variable() {
        let text = "var BasketMgr = require('dw/order/BasketMgr');\nvar cart = BasketMgr.getCurrentBasket();\ncart.getProductLineItems(";
        let help = help(
            Path::new("Cart.js"),
            text,
            text.chars().count(),
            &Workspace::default(),
            &HashMap::new(),
        )
        .unwrap();
        assert!(help.signatures[0].label.starts_with("getProductLineItems("));
    }
}
