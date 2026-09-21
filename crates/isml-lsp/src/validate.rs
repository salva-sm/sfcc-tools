//! The SFCC configuration files nothing else checks.
//!
//! A form definition and a `steptypes.json` are read by the platform at run
//! time, so a typo in a resource key surfaces as a raw key on the page and a
//! wrong `module` path as a job that fails the first time someone runs it.
//! Both are decidable from the checkout.

use std::path::Path;

use lsp_types::{Diagnostic, DiagnosticSeverity, Position, Range};

use crate::workspace::Workspace;

const SOURCE: &str = "sfcc";

/// Attributes of a form field that hold a resource key rather than a value.
const KEY_ATTRIBUTES: [&str; 5] = [
    "label",
    "missing-error",
    "range-error",
    "value-error",
    "parse-error",
];

/// Checks for the configuration files this recognises — form definitions
/// and `steptypes.json`. Empty for anything else.
pub fn diagnostics(file: &Path, text: &str, workspace: &Workspace) -> Vec<Diagnostic> {
    if is_form(file) {
        return form(text, workspace);
    }
    if file
        .file_name()
        .is_some_and(|name| name == "steptypes.json")
    {
        return step_types(file, text);
    }
    Vec::new()
}

fn is_form(file: &Path) -> bool {
    let path = file.to_string_lossy().replace('\\', "/");
    path.contains("/cartridge/forms/") && path.ends_with(".xml")
}

fn form(text: &str, workspace: &Workspace) -> Vec<Diagnostic> {
    let keys = workspace.resource_keys();
    // No bundle in the folder means every key is unknown, which would paint
    // the file. Say nothing instead.
    if keys.is_empty() {
        return Vec::new();
    }

    let mut found = Vec::new();
    for (number, line) in text.lines().enumerate() {
        for attribute in KEY_ATTRIBUTES {
            let Some(value) = attribute_value(line, attribute) else {
                continue;
            };
            if !looks_like_a_key(&value.text) || keys.contains(&value.text) {
                continue;
            }
            found.push(warning(
                number as u32,
                value.start,
                value.text.chars().count(),
                format!(
                    "No bundle defines `{}`, so `{attribute}` renders as the key itself.",
                    value.text
                ),
            ));
        }
    }
    found
}

/// A `label` may hold a literal — a month number, a card brand, a place name.
/// Only a dotted, unspaced value is claiming to be a resource key.
fn looks_like_a_key(value: &str) -> bool {
    value.contains('.')
        && !value.contains(char::is_whitespace)
        && value
            .chars()
            .all(|c| c.is_alphanumeric() || c == '.' || c == '_' || c == '-')
}

fn step_types(file: &Path, text: &str) -> Vec<Diagnostic> {
    let Some(cartridges) = cartridges_dir(file) else {
        return Vec::new();
    };

    let mut found = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for (number, line) in text.lines().enumerate() {
        if let Some(value) = json_string(line, "module") {
            if !resolves(cartridges, &value.text) {
                found.push(warning(
                    number as u32,
                    value.start,
                    value.text.chars().count(),
                    format!(
                        "No file at `{}`; this step fails when the job runs.",
                        value.text
                    ),
                ));
            }
        }
        if let Some(value) = json_string(line, "@type-id") {
            if seen.contains(&value.text) {
                found.push(warning(
                    number as u32,
                    value.start,
                    value.text.chars().count(),
                    format!(
                        "`{}` is declared twice in this file; the second is ignored.",
                        value.text
                    ),
                ));
            }
            seen.push(value.text);
        }
    }
    found
}

/// `steptypes.json` sits at the cartridge root and its `module` paths start at
/// the cartridge name, so they resolve against the directory holding them all.
fn cartridges_dir(file: &Path) -> Option<&Path> {
    file.parent()?.parent()
}

/// The extension is optional in a `module` path, as it is in a `require`.
fn resolves(cartridges: &Path, module: &str) -> bool {
    let relative = module.replace('/', std::path::MAIN_SEPARATOR_STR);
    let direct = cartridges.join(&relative);
    if direct.is_file() {
        return true;
    }
    ["js", "ds"]
        .iter()
        .any(|extension| cartridges.join(format!("{relative}.{extension}")).is_file())
}

struct Value {
    text: String,
    /// Character offset of the value in the line.
    start: usize,
}

/// `attribute="value"`, with where the value starts.
fn attribute_value(line: &str, attribute: &str) -> Option<Value> {
    let marker = format!("{attribute}=\"");
    let at = line.find(&marker)?;
    // `range-error` must not match inside `missing-error`.
    let before = line[..at].chars().next_back();
    if before.is_some_and(|c| c.is_alphanumeric() || c == '-') {
        return None;
    }
    let start = at + marker.len();
    let end = line[start..].find('"')? + start;
    Some(Value {
        text: line[start..end].to_string(),
        start: line[..start].chars().count(),
    })
}

/// `"name": "value"` on one line, with where the value starts.
fn json_string(line: &str, name: &str) -> Option<Value> {
    let marker = format!("\"{name}\"");
    let at = line.find(&marker)?;
    let rest = &line[at + marker.len()..];
    let colon = rest.find(':')?;
    let quote = rest[colon..].find('"')? + colon + 1;
    let end = rest[quote..].find('"')? + quote;
    let start = at + marker.len() + quote;
    Some(Value {
        text: rest[quote..end].to_string(),
        start: line[..start].chars().count(),
    })
}

fn warning(line: u32, column: usize, length: usize, message: String) -> Diagnostic {
    let start = Position::new(line, column as u32);
    Diagnostic {
        range: Range::new(start, Position::new(line, (column + length) as u32)),
        severity: Some(DiagnosticSeverity::WARNING),
        source: Some(SOURCE.to_string()),
        message,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_an_attribute_and_where_its_value_starts() {
        let line = r#"        label="label.input.firstname.profile""#;
        let value = attribute_value(line, "label").unwrap();
        assert_eq!(value.text, "label.input.firstname.profile");
        assert_eq!(&line[value.start..value.start + 5], "label");
    }

    #[test]
    fn does_not_mistake_missing_error_for_range_error() {
        let line = r#"  missing-error="error.required" "#;
        assert!(attribute_value(line, "range-error").is_none());
        assert!(attribute_value(line, "missing-error").is_some());
    }

    #[test]
    fn tells_a_resource_key_from_a_literal_label() {
        assert!(looks_like_a_key("error.message.required"));
        assert!(!looks_like_a_key("01"));
        assert!(!looks_like_a_key("Amex"));
        assert!(!looks_like_a_key("Add to list"));
        assert!(!looks_like_a_key("Alpes-Maritimes"));
    }

    #[test]
    fn reads_a_json_string_field() {
        let line = r#"    "module": "app_brand/cartridge/scripts/jobs/x.js","#;
        let value = json_string(line, "module").unwrap();
        assert_eq!(value.text, "app_brand/cartridge/scripts/jobs/x.js");
        assert_eq!(value.start, line.find("app_brand").unwrap());
    }

    #[test]
    fn accepts_a_module_path_written_without_its_extension() {
        let cartridges = std::env::temp_dir().join("isml-lsp-steps");
        let directory = cartridges.join("app/cartridge/scripts");
        let _ = std::fs::create_dir_all(&directory);
        std::fs::write(
            directory.join("Step.js"),
            "exports.execute = function () {};",
        )
        .unwrap();
        assert!(resolves(&cartridges, "app/cartridge/scripts/Step"));
        assert!(resolves(&cartridges, "app/cartridge/scripts/Step.js"));
        assert!(!resolves(&cartridges, "app/cartridge/scripts/Missing"));
    }

    #[test]
    fn recognises_which_files_it_has_an_opinion_about() {
        assert!(is_form(Path::new(
            r"C:\repo\source\cartridges\app\cartridge\forms\default\profile.xml"
        )));
        assert!(!is_form(Path::new(r"C:\repo\metadata\meta\system.xml")));
    }
}
