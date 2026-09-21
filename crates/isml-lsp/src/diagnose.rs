//! Custom attributes that the metadata in the repository does not define.
//!
//! A typo in `product.custom.season` costs a deploy and a page load to
//! find, because SFCC returns `undefined` rather than failing. The metadata
//! says which names exist, so the typo can be shown while it is being typed.

use lsp_types::{Diagnostic, DiagnosticSeverity, Position, Range};

use crate::custom::{self, Access};
use crate::metadata::{Metadata, SITE_PREFERENCES};

const SOURCE: &str = "sfcc-metadata";

/// Every `.custom.` access and site preference in the document that the
/// metadata does not define. Empty when no metadata was found.
pub fn diagnostics(text: &str, metadata: &Metadata) -> Vec<Diagnostic> {
    // No metadata checked out means every attribute is unknown, which would
    // paint the whole file. Say nothing instead.
    if !metadata.is_loaded() {
        return Vec::new();
    }

    let mut found = Vec::new();
    for (number, line) in text.lines().enumerate() {
        let accesses = custom::accesses(line)
            .into_iter()
            .chain(custom::preferences(line));
        for access in accesses {
            // A type the metadata never extends says nothing about the
            // attribute; one that defines it settles the matter.
            let known: Vec<&str> = access
                .types
                .iter()
                .copied()
                .filter(|type_id| metadata.knows(type_id))
                .collect();
            if known.is_empty() {
                continue;
            }
            if known
                .iter()
                .any(|type_id| metadata.attribute(type_id, &access.attribute).is_some())
            {
                continue;
            }
            found.push(unknown(line, number as u32, &access));
        }
    }
    found
}

fn unknown(line: &str, number: u32, access: &Access) -> Diagnostic {
    let start = utf16_column(line, access.start);
    let end = start + access.attribute.encode_utf16().count() as u32;
    Diagnostic {
        range: Range::new(Position::new(number, start), Position::new(number, end)),
        severity: Some(DiagnosticSeverity::WARNING),
        source: Some(SOURCE.to_string()),
        message: message(access),
        ..Default::default()
    }
}

fn message(access: &Access) -> String {
    if access.types == [SITE_PREFERENCES] {
        return format!(
            "No site preference `{}` is defined in the metadata.",
            access.attribute
        );
    }
    format!(
        "`{}` is not a custom attribute of {} in the metadata.",
        access.attribute,
        access
            .types
            .iter()
            .map(|type_id| format!("`{type_id}`"))
            .collect::<Vec<_>>()
            .join(" or ")
    )
}

fn utf16_column(line: &str, character: usize) -> u32 {
    line.chars()
        .take(character)
        .map(char::len_utf16)
        .sum::<usize>() as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<metadata xmlns="http://www.demandware.com/xml/impex/metadata/2006-10-31">
    <type-extension type-id="Product">
        <custom-attribute-definitions>
            <attribute-definition attribute-id="season"><type>string</type></attribute-definition>
        </custom-attribute-definitions>
    </type-extension>
    <type-extension type-id="SitePreferences">
        <custom-attribute-definitions>
            <attribute-definition attribute-id="newsletterEnabled"><type>boolean</type></attribute-definition>
        </custom-attribute-definitions>
    </type-extension>
</metadata>
"#;

    /// Tests run in parallel: sharing one directory means one test can scan
    /// the file another is still writing.
    fn metadata(test: &str) -> Metadata {
        let directory = std::env::temp_dir().join(format!("isml-lsp-diagnose-{test}"));
        let _ = fs::create_dir_all(&directory);
        fs::write(directory.join("system-objecttype-extensions.xml"), SAMPLE).unwrap();
        Metadata::scan(&[directory])
    }

    #[test]
    fn accepts_an_attribute_the_metadata_defines() {
        let metadata = metadata("accepts");
        assert!(diagnostics("var d = product.custom.season;", &metadata).is_empty());
    }

    #[test]
    fn reports_a_typo_where_it_is() {
        let metadata = metadata("typo");
        let line = "var d = product.custom.seasson;";
        let found = diagnostics(line, &metadata);
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].range.start.character as usize,
            line.find("seasson").unwrap()
        );
        assert!(found[0].message.contains("Product"));
    }

    #[test]
    fn reports_an_undefined_site_preference() {
        let metadata = metadata("preference");
        let found = diagnostics(
            "Site.getCurrent().getCustomPreferenceValue('newsletterEnabledd')",
            &metadata,
        );
        assert_eq!(found.len(), 1);
        assert!(found[0].message.contains("site preference"));
    }

    #[test]
    fn stays_quiet_on_a_type_the_metadata_never_extends() {
        let metadata = metadata("unknown-type");
        assert!(diagnostics("var x = coupon.custom.whatever;", &metadata).is_empty());
    }

    #[test]
    fn stays_quiet_without_metadata() {
        let empty = Metadata::scan(&[std::env::temp_dir().join("isml-lsp-absent")]);
        assert!(diagnostics("var d = product.custom.nope;", &empty).is_empty());
    }
}
