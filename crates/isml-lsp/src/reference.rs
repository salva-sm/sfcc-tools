//! Finding the reference under the cursor in an ISML or JavaScript line.

#[derive(Debug, PartialEq, Eq)]
pub enum Reference {
    /// `<isinclude template="account/dashboard"/>`
    Template(String),
    /// `require('*/cartridge/scripts/helpers/brandHelper')`
    Module(String),
    /// `Resource.msg('label.profile.firstname', 'account', null)`
    Resource { key: String, bundle: String },
}

/// The reference the cursor sits on, if any. `column` is a character offset
/// into `line`.
pub fn at_cursor(line: &str, column: usize) -> Option<Reference> {
    let chars: Vec<char> = line.chars().collect();
    let column = column.min(chars.len());

    let literal = string_literal_at(&chars, column)?;
    let before = chars[..literal.start].iter().collect::<String>();

    if let Some(reference) = resource_call(&chars, column) {
        return Some(reference);
    }
    if ends_with_call(&before, "require") {
        return Some(Reference::Module(literal.text));
    }
    if is_template_attribute(&before) {
        return Some(Reference::Template(literal.text));
    }
    None
}

struct StringLiteral {
    text: String,
    /// Index of the opening quote.
    start: usize,
}

/// The quoted literal containing `column`, scanning the line from the left so
/// that quotes nested inside an outer attribute value resolve to the inner one.
fn string_literal_at(chars: &[char], column: usize) -> Option<StringLiteral> {
    let mut index = 0;
    while index < chars.len() {
        let quote = chars[index];
        if quote != '\'' && quote != '"' {
            index += 1;
            continue;
        }
        let content_start = index + 1;
        let mut end = content_start;
        while end < chars.len() && chars[end] != quote {
            end += 1;
        }
        if end >= chars.len() {
            return None;
        }
        if (content_start..=end).contains(&column) {
            // `value="${require('x')}"` — prefer the inner literal the cursor
            // is actually on over the attribute value wrapping it.
            if let Some(nested) =
                string_literal_at(&chars[content_start..end], column - content_start)
            {
                return Some(StringLiteral {
                    text: nested.text,
                    start: content_start + nested.start,
                });
            }
            return Some(StringLiteral {
                text: chars[content_start..end].iter().collect(),
                start: index,
            });
        }
        index = end + 1;
    }
    None
}

/// True when `before` ends with `name(` plus optional whitespace.
fn ends_with_call(before: &str, name: &str) -> bool {
    let trimmed = before.trim_end();
    let Some(head) = trimmed.strip_suffix('(') else {
        return false;
    };
    let head = head.trim_end();
    if !head.ends_with(name) {
        return false;
    }
    let preceding = head[..head.len() - name.len()].chars().next_back();
    !matches!(preceding, Some(c) if c.is_alphanumeric() || c == '_' || c == '$')
}

fn is_template_attribute(before: &str) -> bool {
    let trimmed = before.trim_end();
    let Some(head) = trimmed.strip_suffix('=') else {
        return false;
    };
    head.trim_end().to_ascii_lowercase().ends_with("template")
}

/// `Resource.msg('key', 'bundle', null)`, `Resource.msgf(...)` and the Lit
/// client equivalent `i18nMessage('key', 'bundle')`. Either argument resolves
/// to the same place, so the whole call is matched rather than one literal.
fn resource_call(chars: &[char], column: usize) -> Option<Reference> {
    let line: String = chars.iter().collect();
    for open in call_openings(&line) {
        if column < open {
            continue;
        }
        let close = matching_paren(chars, open)?;
        if column > close {
            continue;
        }
        let args = string_arguments(&chars[open + 1..close]);
        if args.len() < 2 {
            continue;
        }
        return Some(Reference::Resource {
            key: args[0].clone(),
            bundle: args[1].clone(),
        });
    }
    None
}

/// Offsets of the `(` that opens a localisation call.
fn call_openings(line: &str) -> Vec<usize> {
    const NAMES: [&str; 3] = ["Resource.msgf", "Resource.msg", "i18nMessage"];
    let chars: Vec<char> = line.chars().collect();
    let mut openings = Vec::new();
    for (index, c) in chars.iter().enumerate() {
        if *c != '(' {
            continue;
        }
        let head: String = chars[..index].iter().collect::<String>().trim_end().into();
        if NAMES.iter().any(|name| head.ends_with(name)) {
            openings.push(index);
        }
    }
    openings
}

fn matching_paren(chars: &[char], open: usize) -> Option<usize> {
    let mut depth = 0;
    let mut index = open;
    while index < chars.len() {
        match chars[index] {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
            }
            '\'' | '"' => {
                let quote = chars[index];
                index += 1;
                while index < chars.len() && chars[index] != quote {
                    index += 1;
                }
            }
            _ => {}
        }
        index += 1;
    }
    None
}

fn string_arguments(chars: &[char]) -> Vec<String> {
    let mut arguments = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        let quote = chars[index];
        if quote == '\'' || quote == '"' {
            let start = index + 1;
            let mut end = start;
            while end < chars.len() && chars[end] != quote {
                end += 1;
            }
            arguments.push(chars[start..end.min(chars.len())].iter().collect());
            index = end + 1;
        } else {
            index += 1;
        }
    }
    arguments
}

#[cfg(test)]
mod tests {
    use super::*;

    fn column_of(line: &str, needle: &str) -> usize {
        line.find(needle).unwrap() + 1
    }

    #[test]
    fn finds_a_template_attribute() {
        let line = r#"    <isinclude template="account/dashboard" />"#;
        assert_eq!(
            at_cursor(line, column_of(line, "account/dashboard")),
            Some(Reference::Template("account/dashboard".into()))
        );
    }

    #[test]
    fn finds_a_require_path() {
        let line = r#"var helper = require('*/cartridge/scripts/helpers/brandHelper');"#;
        assert_eq!(
            at_cursor(line, column_of(line, "*/cartridge")),
            Some(Reference::Module(
                "*/cartridge/scripts/helpers/brandHelper".into()
            ))
        );
    }

    #[test]
    fn finds_a_require_inside_an_isml_expression() {
        let line =
            r#"<isset name="a" value="${require('~/cartridge/scripts/x').y}" scope="page"/>"#;
        assert_eq!(
            at_cursor(line, column_of(line, "~/cartridge")),
            Some(Reference::Module("~/cartridge/scripts/x".into()))
        );
    }

    #[test]
    fn finds_a_resource_key_from_either_argument() {
        let line = r#"<isprint value="${Resource.msg('label.x.y', 'account', null)}"/>"#;
        let expected = Reference::Resource {
            key: "label.x.y".into(),
            bundle: "account".into(),
        };
        assert_eq!(
            at_cursor(line, column_of(line, "label.x.y")),
            Some(expected)
        );
        assert!(matches!(
            at_cursor(line, column_of(line, "account")),
            Some(Reference::Resource { .. })
        ));
    }

    #[test]
    fn ignores_an_unrelated_string() {
        let line = r#"<div class="product-tile"></div>"#;
        assert_eq!(at_cursor(line, column_of(line, "product-tile")), None);
    }

    #[test]
    fn ignores_a_lookalike_identifier_before_the_parenthesis() {
        let line = r#"var x = myrequire('./a');"#;
        assert_eq!(at_cursor(line, column_of(line, "./a")), None);
    }
}
