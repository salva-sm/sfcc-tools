//! What to offer at the cursor.
//!
//! Deciding *where* the cursor is comes first — [`context_at`] — and what to
//! put there second. The contexts are the ones a general editor gets wrong or
//! cannot see: the ISML tag set (HTML offers `<is:include>`, which does not
//! exist), tag attributes and their values, template paths, route names, the
//! `dw.*` API, and the custom attributes the instance actually defines.

use std::path::Path;

use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionTextEdit, Documentation, InsertTextFormat,
    MarkupContent, MarkupKind, Position, Range, TextEdit,
};

use crate::api;
use crate::custom;
use crate::isml::{self, Body};
use crate::metadata::{Metadata, SITE_PREFERENCES};
use crate::routes::Route;
use crate::workspace::Workspace;

const MAX_TEMPLATES: usize = 2000;

/// What the cursor is in the middle of writing.
///
/// Every variant carries `typed`, the number of characters already there, so
/// the completion replaces them instead of appending to them.
#[derive(Debug, PartialEq, Eq)]
pub enum Context {
    /// Just after `<`.
    TagName {
        /// Characters of the name already typed.
        typed: usize,
    },
    /// Inside an open tag, where an attribute goes.
    AttributeName {
        /// The tag being written.
        tag: String,
        /// Characters of the attribute name already typed.
        typed: usize,
    },
    /// Inside the quotes of an attribute.
    AttributeValue {
        /// The tag being written.
        tag: String,
        /// The attribute whose value it is.
        attribute: String,
        /// Characters of the value already typed.
        typed: usize,
    },
    /// After `.custom.` on something with a known object type.
    CustomAttribute(custom::Pending),
    /// Inside the argument of `getCustomPreferenceValue`.
    SitePreference {
        /// Characters already typed.
        typed: usize,
    },
    /// The first argument of `server.<verb>(`, which names a route.
    RouteName {
        /// Characters already typed.
        typed: usize,
    },
    /// Inside `require('`, where a `dw/...` module may go.
    DwModule {
        /// Characters already typed.
        typed: usize,
    },
    /// A bare `dwSite` being typed, which stands for the class plus the
    /// `require` that brings it in.
    DwImport {
        /// What has been typed, `dw` prefix included.
        typed: String,
    },
    /// `var Trans` at the head of a file, where a `require` is being written.
    DwDeclaration {
        /// `var`, `const` or `let`, as written.
        keyword: String,
        /// The class name typed so far.
        typed: String,
    },
    /// After the dot on an identifier bound to an API class by `require`.
    DwMember {
        /// The qualified class the receiver stands for.
        class: String,
        /// Characters already typed.
        typed: usize,
    },
}

/// The context at a character offset into the document.
pub fn context_at(text: &str, offset: usize, is_isml: bool) -> Option<Context> {
    let chars: Vec<char> = text.chars().collect();
    let offset = offset.min(chars.len());
    let head: String = chars[..offset].iter().collect();

    if let Some(pending) = custom::pending(&head) {
        return Some(Context::CustomAttribute(pending));
    }
    if custom::is_pending_preference(&head) {
        return Some(Context::SitePreference {
            typed: typed_since_quote(&head),
        });
    }
    if is_pending_route(&head) {
        return Some(Context::RouteName {
            typed: typed_since_quote(&head),
        });
    }
    if is_pending_require(&head) {
        return Some(Context::DwModule {
            typed: typed_since_quote(&head),
        });
    }
    if let Some(member) = pending_member(&head, text) {
        return Some(member);
    }
    // The declaration form is checked first: `var dwSite` is being written as
    // a declaration, not as a bare reference.
    if let Some(declaration) = pending_declaration(&head) {
        return Some(declaration);
    }
    if let Some(typed) = pending_import(&head) {
        return Some(Context::DwImport { typed });
    }
    if is_isml {
        return tag_context(&chars, offset);
    }
    None
}

fn tag_context(chars: &[char], offset: usize) -> Option<Context> {
    let open = open_tag_start(chars, offset)?;
    let inner: Vec<char> = chars[open + 1..offset].to_vec();
    if inner.first().is_some_and(|c| *c == '/' || *c == '!') {
        return None;
    }

    let Some(name_end) = inner.iter().position(|c| c.is_whitespace()) else {
        return Some(Context::TagName { typed: inner.len() });
    };
    let tag: String = inner[..name_end].iter().collect();
    if !tag.starts_with("is") {
        return None;
    }

    let rest = &inner[name_end..];
    match open_quote(rest) {
        Some(quote) => Some(Context::AttributeValue {
            tag,
            attribute: attribute_before(&rest[..quote]),
            typed: rest.len() - quote - 1,
        }),
        None => Some(Context::AttributeName {
            tag,
            typed: trailing_name_len(rest),
        }),
    }
}

/// Offset of the `<` that opens the tag the cursor is inside, if it is.
fn open_tag_start(chars: &[char], offset: usize) -> Option<usize> {
    let mut index = offset;
    while index > 0 {
        index -= 1;
        match chars[index] {
            '>' => return None,
            '<' => return Some(index),
            _ => {}
        }
    }
    None
}

/// Offset of the quote opening the value the cursor sits in, if it is unclosed.
fn open_quote(chars: &[char]) -> Option<usize> {
    let mut open: Option<usize> = None;
    for (index, c) in chars.iter().enumerate() {
        if *c != '\'' && *c != '"' {
            continue;
        }
        match open {
            Some(start) if chars[start] == *c => open = None,
            Some(_) => {}
            None => open = Some(index),
        }
    }
    open
}

fn attribute_before(chars: &[char]) -> String {
    let mut end = chars.len();
    while end > 0 && (chars[end - 1].is_whitespace() || chars[end - 1] == '=') {
        end -= 1;
    }
    let mut start = end;
    while start > 0 && (chars[start - 1].is_alphanumeric() || chars[start - 1] == '-') {
        start -= 1;
    }
    chars[start..end].iter().collect()
}

fn trailing_name_len(chars: &[char]) -> usize {
    let mut length = 0;
    while length < chars.len() {
        let c = chars[chars.len() - 1 - length];
        if !c.is_alphanumeric() && c != '-' {
            break;
        }
        length += 1;
    }
    length
}

/// True when the cursor sits in the route argument of a `server.<verb>(`.
fn is_pending_route(head: &str) -> bool {
    const VERBS: [&str; 6] = ["get", "post", "use", "append", "prepend", "replace"];
    let Some(quote) = head.rfind(['\'', '"']) else {
        return false;
    };
    let before = head[..quote].trim_end();
    let Some(call) = before.strip_suffix('(') else {
        return false;
    };
    let call = call.trim_end();
    VERBS
        .iter()
        .any(|verb| call.ends_with(&format!("server.{verb}")))
}

fn is_pending_require(head: &str) -> bool {
    let Some(quote) = head.rfind(['\'', '"']) else {
        return false;
    };
    let before = head[..quote].trim_end();
    before
        .strip_suffix('(')
        .is_some_and(|call| call.trim_end().ends_with("require"))
}

/// `Site.` where the document bound `Site` to an API class.
fn pending_member(head: &str, text: &str) -> Option<Context> {
    let typed: String = head
        .chars()
        .rev()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
        .collect();
    let stem = &head[..head.len() - typed.len()];
    let receiver: String = stem
        .strip_suffix('.')?
        .chars()
        .rev()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let class = api::bindings(text).get(&receiver)?.clone();
    Some(Context::DwMember {
        class,
        typed: typed.chars().count(),
    })
}

/// `var Trans` — a declaration whose right-hand side is still missing.
fn pending_declaration(head: &str) -> Option<Context> {
    let line = head.rsplit('\n').next()?;
    let typed = trailing_word(line);
    let before = line[..line.len() - typed.len()].trim_end();
    let keyword = ["var", "const", "let"]
        .into_iter()
        .find(|word| before.ends_with(word))?;
    // Only at the start of a statement: `x = var` is not a declaration, and
    // neither is an identifier that merely ends in those letters.
    let head_of_line = before[..before.len() - keyword.len()].trim();
    if !head_of_line.is_empty() {
        return None;
    }
    Some(Context::DwDeclaration {
        keyword: keyword.to_string(),
        typed,
    })
}

/// A bare word starting with `dw`, which is the shorthand for importing a class.
fn pending_import(head: &str) -> Option<String> {
    let typed = trailing_word(head);
    if typed.len() < 2 || !typed.to_ascii_lowercase().starts_with("dw") {
        return None;
    }
    // `x.dwSite` is a member access, not a name being introduced.
    let before = &head[..head.len() - typed.len()];
    match before.chars().next_back() {
        Some('.') => None,
        _ => Some(typed),
    }
}

fn trailing_word(text: &str) -> String {
    text.chars()
        .rev()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

fn typed_since_quote(head: &str) -> usize {
    match head.rfind(['\'', '"']) {
        Some(quote) => head[quote + 1..].chars().count(),
        None => 0,
    }
}

/// Everything an answer may need to draw on.
pub struct Completer<'a> {
    /// The cartridges, and the indexes built from them.
    pub workspace: &'a Workspace,
    /// The custom attributes the checkout declares.
    pub metadata: &'a Metadata,
    /// The document being edited, which decides where a new `require` goes.
    pub text: &'a str,
    /// The file being edited, which names the controller a bare route belongs to.
    pub file: &'a Path,
}

impl Completer<'_> {
    /// What to offer for a context, each item replacing what is typed.
    pub fn items(&self, context: &Context, cursor: Position) -> Vec<CompletionItem> {
        match context {
            Context::TagName { typed } => tag_items(replaced(cursor, *typed)),
            Context::AttributeName { tag, typed } => attribute_items(tag, replaced(cursor, *typed)),
            Context::AttributeValue {
                tag,
                attribute,
                typed,
            } => self.value_items(tag, attribute, replaced(cursor, *typed)),
            Context::CustomAttribute(pending) => {
                self.custom_items(pending.types, replaced(cursor, pending.typed))
            }
            Context::SitePreference { typed } => {
                self.custom_items(&[SITE_PREFERENCES], replaced(cursor, *typed))
            }
            Context::RouteName { typed } => self.route_items(replaced(cursor, *typed)),
            Context::DwModule { typed } => module_items(replaced(cursor, *typed)),
            Context::DwImport { typed } => {
                self.import_items(replaced(cursor, typed.chars().count()))
            }
            Context::DwDeclaration { keyword, typed } => {
                declaration_items(replaced(cursor, typed.chars().count()), keyword, typed)
            }
            Context::DwMember { class, typed } => member_items(class, replaced(cursor, *typed)),
        }
    }

    /// The routes this controller already has somewhere in the path — what a
    /// `server.append` in this file could legally attach to.
    fn route_items(&self, range: Range) -> Vec<CompletionItem> {
        let Some(controller) = self.file.file_stem().and_then(|stem| stem.to_str()) else {
            return Vec::new();
        };
        self.workspace
            .controllers()
            .routes_of(controller)
            .into_iter()
            .map(|route| CompletionItem {
                label: route.to_string(),
                kind: Some(CompletionItemKind::METHOD),
                detail: Some(
                    Route {
                        controller: controller.to_string(),
                        name: route.to_string(),
                    }
                    .endpoint(),
                ),
                text_edit: Some(edit(range, route.to_string())),
                ..Default::default()
            })
            .collect()
    }

    fn value_items(&self, tag: &str, attribute: &str, range: Range) -> Vec<CompletionItem> {
        if attribute == isml::TEMPLATE_ATTRIBUTE {
            return self.template_items(range);
        }
        let Some(definition) = isml::tag(tag) else {
            return Vec::new();
        };
        let Some(found) = definition
            .attributes
            .iter()
            .find(|candidate| candidate.name == attribute)
        else {
            return Vec::new();
        };
        found
            .values
            .iter()
            .map(|value| CompletionItem {
                label: value.to_string(),
                kind: Some(CompletionItemKind::VALUE),
                text_edit: Some(edit(range, value.to_string())),
                ..Default::default()
            })
            .collect()
    }

    fn template_items(&self, range: Range) -> Vec<CompletionItem> {
        self.workspace
            .templates()
            .iter()
            .take(MAX_TEMPLATES)
            .map(|path| CompletionItem {
                label: path.clone(),
                kind: Some(CompletionItemKind::FILE),
                text_edit: Some(edit(range, path.clone())),
                ..Default::default()
            })
            .collect()
    }

    /// The union over the candidate types, since a name that could be either
    /// of two objects can legally carry the attributes of both.
    fn custom_items(&self, types: &[&str], range: Range) -> Vec<CompletionItem> {
        let mut items: Vec<CompletionItem> = Vec::new();
        for type_id in types {
            let Some(attributes) = self.metadata.attributes_of(type_id) else {
                continue;
            };
            for definition in attributes.values() {
                if items.iter().any(|item| item.label == definition.id) {
                    continue;
                }
                items.push(CompletionItem {
                    label: definition.id.clone(),
                    kind: Some(CompletionItemKind::FIELD),
                    detail: Some(definition.detail()),
                    documentation: definition.documentation().map(markdown),
                    text_edit: Some(edit(range, definition.id.clone())),
                    ..Default::default()
                });
            }
        }
        items
    }
}

impl Completer<'_> {
    /// `dwSite` becomes `Site`, with `var Site = require('dw/system/Site');`
    /// added to the require block in the same keystroke — unless the document
    /// already has it, in which case only the name is inserted.
    fn import_items(&self, range: Range) -> Vec<CompletionItem> {
        let imports = api::imports(self.text);
        api::api()
            .classes()
            .map(|(qualified, class)| {
                let short = short_name(qualified);
                let bound = imports.name_of(qualified);
                let module = qualified.replace('.', "/");
                CompletionItem {
                    label: format!("dw{short}"),
                    kind: Some(CompletionItemKind::CLASS),
                    detail: Some(match bound {
                        Some(_) => format!("{module} (already required)"),
                        None => module.clone(),
                    }),
                    documentation: (!class.description.is_empty())
                        .then(|| markdown(class.description.clone())),
                    text_edit: Some(edit(range, bound.unwrap_or(short).to_string())),
                    additional_text_edits: bound
                        .is_none()
                        .then(|| vec![require_line(&imports, short, &module)]),
                    ..Default::default()
                }
            })
            .collect()
    }
}

/// `var Trans` completes to the whole declaration, in place — no second edit
/// needed, because the cursor is already where the `require` belongs.
fn declaration_items(range: Range, keyword: &str, typed: &str) -> Vec<CompletionItem> {
    let shorthand = typed.to_ascii_lowercase().starts_with("dw");
    api::api()
        .classes()
        .map(|(qualified, class)| {
            let short = short_name(qualified);
            let module = qualified.replace('.', "/");
            CompletionItem {
                label: short.to_string(),
                kind: Some(CompletionItemKind::CLASS),
                detail: Some(format!("{keyword} {short} = require('{module}');")),
                documentation: (!class.description.is_empty())
                    .then(|| markdown(class.description.clone())),
                // Typing `var dwSite` should still find it, so the filter
                // follows whichever spelling is being used.
                filter_text: shorthand.then(|| format!("dw{short}")),
                text_edit: Some(edit(range, format!("{short} = require('{module}');"))),
                ..Default::default()
            }
        })
        .collect()
}

fn require_line(imports: &api::Imports, name: &str, module: &str) -> TextEdit {
    let at = Position::new(imports.line, 0);
    TextEdit {
        range: Range::new(at, at),
        new_text: format!("{} {name} = require('{module}');\n", imports.keyword),
    }
}

fn short_name(qualified: &str) -> &str {
    qualified.rsplit('.').next().unwrap_or(qualified)
}

fn module_items(range: Range) -> Vec<CompletionItem> {
    api::api()
        .modules()
        .map(|(path, class)| CompletionItem {
            label: path.clone(),
            kind: Some(CompletionItemKind::MODULE),
            documentation: (!class.description.is_empty())
                .then(|| markdown(class.description.clone())),
            text_edit: Some(edit(range, path)),
            ..Default::default()
        })
        .collect()
}

fn member_items(class: &str, range: Range) -> Vec<CompletionItem> {
    let Some(found) = api::api().class(class) else {
        return Vec::new();
    };
    found
        .members()
        .map(|(member, kind)| CompletionItem {
            label: member.name.clone(),
            kind: Some(match kind {
                api::MemberKind::Method => CompletionItemKind::METHOD,
                api::MemberKind::Property => CompletionItemKind::PROPERTY,
                api::MemberKind::Constant => CompletionItemKind::CONSTANT,
            }),
            detail: Some(member.shape.clone()),
            documentation: (!member.description.is_empty())
                .then(|| markdown(member.description.clone())),
            text_edit: Some(edit(range, member.name.clone())),
            ..Default::default()
        })
        .collect()
}

fn tag_items(range: Range) -> Vec<CompletionItem> {
    isml::tags()
        .iter()
        .map(|tag| CompletionItem {
            label: tag.name.to_string(),
            kind: Some(CompletionItemKind::KEYWORD),
            detail: Some(tag.summary.to_string()),
            insert_text_format: Some(InsertTextFormat::SNIPPET),
            text_edit: Some(edit(range, snippet(tag))),
            ..Default::default()
        })
        .collect()
}

fn attribute_items(tag: &str, range: Range) -> Vec<CompletionItem> {
    let Some(definition) = isml::tag(tag) else {
        return Vec::new();
    };
    definition
        .attributes
        .iter()
        .map(|attribute| CompletionItem {
            label: attribute.name.to_string(),
            kind: Some(CompletionItemKind::PROPERTY),
            detail: Some(match attribute.required {
                true => format!("required — {}", attribute.summary),
                false => attribute.summary.to_string(),
            }),
            sort_text: Some(match attribute.required {
                true => format!("0{}", attribute.name),
                false => format!("1{}", attribute.name),
            }),
            insert_text_format: Some(InsertTextFormat::SNIPPET),
            text_edit: Some(edit(range, format!("{}=\"$1\"", attribute.name))),
            ..Default::default()
        })
        .collect()
}

/// The whole tag, so accepting `isif` leaves a usable skeleton rather than a
/// bare name the developer still has to close by hand.
fn snippet(tag: &isml::Tag) -> String {
    let required: Vec<String> = tag
        .attributes
        .iter()
        .filter(|attribute| attribute.required)
        .enumerate()
        .map(|(index, attribute)| format!(" {}=\"${}\"", attribute.name, index + 1))
        .collect();
    let attributes = required.concat();
    // A tag with optional attributes leaves the cursor where one would go.
    let room = match required.is_empty() && !tag.attributes.is_empty() {
        true => " $0",
        false => "",
    };

    match tag.body {
        Body::Empty => format!("{}{}{} />", tag.name, attributes, room),
        Body::Inline => format!("{0}{1}>$0</{0}>", tag.name, attributes),
        Body::Block => format!("{0}{1}>\n\t$0\n</{0}>", tag.name, attributes),
    }
}

fn replaced(cursor: Position, typed: usize) -> Range {
    let start = Position::new(cursor.line, cursor.character.saturating_sub(typed as u32));
    Range::new(start, cursor)
}

fn edit(range: Range, text: String) -> CompletionTextEdit {
    CompletionTextEdit::Edit(TextEdit {
        range,
        new_text: text,
    })
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

    fn context(text: &str) -> Option<Context> {
        context_at(text, text.chars().count(), true)
    }

    #[test]
    fn completes_a_tag_name_after_the_angle_bracket() {
        assert_eq!(context("<isi"), Some(Context::TagName { typed: 3 }));
        assert_eq!(context("  <"), Some(Context::TagName { typed: 0 }));
    }

    #[test]
    fn completes_an_attribute_name_inside_an_open_tag() {
        assert_eq!(
            context("<isloop items=\"${x}\" va"),
            Some(Context::AttributeName {
                tag: "isloop".into(),
                typed: 2
            })
        );
    }

    #[test]
    fn completes_an_attribute_value_inside_the_quotes() {
        assert_eq!(
            context("<isset name=\"a\" value=\"b\" scope=\"ses"),
            Some(Context::AttributeValue {
                tag: "isset".into(),
                attribute: "scope".into(),
                typed: 3
            })
        );
    }

    #[test]
    fn leaves_plain_html_alone() {
        assert_eq!(context("<div cla"), None);
        assert_eq!(context("<isif condition=\"${x}\">text "), None);
    }

    #[test]
    fn spans_a_tag_broken_over_several_lines() {
        assert_eq!(
            context("<isslot\n    id=\"home\"\n    conte"),
            Some(Context::AttributeName {
                tag: "isslot".into(),
                typed: 5
            })
        );
    }

    #[test]
    fn prefers_a_custom_attribute_inside_an_expression() {
        assert_eq!(
            context("<isif condition=\"${product.custom.gue"),
            Some(Context::CustomAttribute(custom::Pending {
                types: &["Product"],
                typed: 3
            }))
        );
    }

    #[test]
    fn completes_a_route_name_in_a_server_declaration() {
        let text = "server.append('Sh";
        assert_eq!(
            context_at(text, text.chars().count(), false),
            Some(Context::RouteName { typed: 2 })
        );
    }

    #[test]
    fn completes_a_declaration_being_written() {
        let text = "'use strict';\nvar Trans";
        assert_eq!(
            context_at(text, text.chars().count(), false),
            Some(Context::DwDeclaration {
                keyword: "var".into(),
                typed: "Trans".into()
            })
        );
    }

    #[test]
    fn takes_the_shorthand_inside_a_declaration_too() {
        let text = "const dwSite";
        assert_eq!(
            context_at(text, text.chars().count(), false),
            Some(Context::DwDeclaration {
                keyword: "const".into(),
                typed: "dwSite".into()
            })
        );
    }

    #[test]
    fn completes_a_bare_shorthand_as_an_import() {
        let text = "    dwSite";
        assert_eq!(
            context_at(text, text.chars().count(), false),
            Some(Context::DwImport {
                typed: "dwSite".into()
            })
        );
    }

    #[test]
    fn leaves_a_word_that_merely_ends_in_a_keyword_alone() {
        let text = "myvar Trans";
        assert!(!matches!(
            context_at(text, text.chars().count(), false),
            Some(Context::DwDeclaration { .. })
        ));
    }

    #[test]
    fn completes_custom_attributes_in_plain_javascript_too() {
        let text = "var x = order.custom.";
        assert_eq!(
            context_at(text, text.chars().count(), false),
            Some(Context::CustomAttribute(custom::Pending {
                types: &["Order"],
                typed: 0
            }))
        );
    }

    #[test]
    fn closes_an_empty_tag_and_leaves_the_cursor_in_the_first_attribute() {
        let isinclude = isml::tag("isinclude").unwrap();
        assert_eq!(snippet(isinclude), "isinclude $0 />");
        let isif = isml::tag("isif").unwrap();
        assert_eq!(snippet(isif), "isif condition=\"$1\">$0</isif>");
    }
}
