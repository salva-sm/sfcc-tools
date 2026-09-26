//! Custom attributes from the metadata XML in the repository, the only local record of what `x.custom.y` may be.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

const MAX_DEPTH: usize = 8;
const SKIPPED: [&str; 7] = [
    "node_modules",
    ".git",
    "static",
    "build",
    "dist",
    "coverage",
    "target",
];

/// The type extension holding the site preferences, which are reached through
/// `getCustomPreferenceValue` rather than a `.custom.` access.
pub const SITE_PREFERENCES: &str = "SitePreferences";

#[derive(Debug, Clone, Default)]
pub struct AttributeDefinition {
    pub id: String,
    /// The label Business Manager shows.
    pub display_name: Option<String>,
    pub value_type: Option<String>,
    pub values: Vec<String>,
    pub mandatory: bool,
    pub localizable: bool,
}

impl AttributeDefinition {
    /// One line for a completion list: what it is, not where it came from.
    pub fn detail(&self) -> String {
        let mut detail = self.value_type.clone().unwrap_or_else(|| "?".into());
        if self.localizable {
            detail.push_str(", localizable");
        }
        if self.mandatory {
            detail.push_str(", mandatory");
        }
        detail
    }

    pub fn documentation(&self) -> Option<String> {
        let mut lines = Vec::new();
        if let Some(name) = &self.display_name {
            lines.push(name.clone());
        }
        if !self.values.is_empty() {
            lines.push(format!("Values: {}", self.values.join(", ")));
        }
        (!lines.is_empty()).then(|| lines.join("\n\n"))
    }
}

#[derive(Debug, Default)]
pub struct Metadata {
    types: BTreeMap<String, BTreeMap<String, AttributeDefinition>>,
    sources: usize,
}

impl Metadata {
    pub fn scan(roots: &[PathBuf]) -> Metadata {
        let mut metadata = Metadata::default();
        for root in roots {
            metadata.visit(root, 0);
        }
        metadata
    }

    /// False when no metadata was found at all, which has to silence every
    /// diagnostic: an unknown attribute and an unknown instance look the same.
    pub fn is_loaded(&self) -> bool {
        self.sources > 0
    }

    pub fn attributes_of(&self, type_id: &str) -> Option<&BTreeMap<String, AttributeDefinition>> {
        self.types.get(type_id)
    }

    pub fn attribute(&self, type_id: &str, attribute_id: &str) -> Option<&AttributeDefinition> {
        self.types.get(type_id)?.get(attribute_id)
    }

    /// Whether the metadata extends this object type at all. A type it never
    /// mentions says nothing about an attribute, so nothing is reported for it.
    pub fn knows(&self, type_id: &str) -> bool {
        self.types.contains_key(type_id)
    }

    fn visit(&mut self, dir: &Path, depth: usize) {
        if depth > MAX_DEPTH {
            return;
        }
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                if !SKIPPED.contains(&name) && !name.starts_with('.') {
                    self.visit(&path, depth + 1);
                }
                continue;
            }
            if is_metadata_file(name) {
                self.absorb(&path);
            }
        }
    }

    fn absorb(&mut self, file: &Path) {
        let Ok(text) = fs::read_to_string(file) else {
            return;
        };
        let Ok(document) = roxmltree::Document::parse(&text) else {
            return;
        };
        self.sources += 1;
        for node in document.root_element().children() {
            match local_name(node) {
                // <type-extension type-id="Product"> extends a system object.
                "type-extension" => self.absorb_definitions(node, "custom-attribute-definitions"),
                // <custom-type type-id="MyObject"> defines a custom object.
                "custom-type" => self.absorb_definitions(node, "attribute-definitions"),
                _ => {}
            }
        }
    }

    fn absorb_definitions(&mut self, type_node: roxmltree::Node, container: &str) {
        let Some(type_id) = type_node.attribute("type-id") else {
            return;
        };
        let entry = self.types.entry(type_id.to_string()).or_default();
        for group in type_node
            .children()
            .filter(|node| local_name(*node) == container)
        {
            for node in group
                .children()
                .filter(|node| local_name(*node) == "attribute-definition")
            {
                if let Some(definition) = read_definition(node) {
                    entry.insert(definition.id.clone(), definition);
                }
            }
        }
    }
}

fn is_metadata_file(name: &str) -> bool {
    name.ends_with("objecttype-extensions.xml") || name.ends_with("objecttype-definitions.xml")
}

fn local_name<'a>(node: roxmltree::Node<'a, 'a>) -> &'a str {
    node.tag_name().name()
}

fn read_definition(node: roxmltree::Node) -> Option<AttributeDefinition> {
    let id = node.attribute("attribute-id")?;
    let mut definition = AttributeDefinition {
        id: id.to_string(),
        ..Default::default()
    };

    for child in node.children().filter(|child| child.is_element()) {
        let text = child.text().map(str::trim).unwrap_or_default();
        match local_name(child) {
            "display-name" if definition.display_name.is_none() => {
                definition.display_name = Some(text.to_string());
            }
            "type" => definition.value_type = Some(text.to_string()),
            "mandatory-flag" => definition.mandatory = text == "true",
            "localizable-flag" => definition.localizable = text == "true",
            "value-definitions" => definition.values = read_values(child),
            _ => {}
        }
    }
    Some(definition)
}

fn read_values(node: roxmltree::Node) -> Vec<String> {
    node.children()
        .filter(|child| local_name(*child) == "value-definition")
        .filter_map(|child| {
            child
                .children()
                .find(|leaf| local_name(*leaf) == "value")
                .and_then(|leaf| leaf.text())
                .map(|text| text.trim().to_string())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<metadata xmlns="http://www.demandware.com/xml/impex/metadata/2006-10-31">
    <type-extension type-id="Product">
        <custom-attribute-definitions>
            <attribute-definition attribute-id="season">
                <display-name xml:lang="x-default">Drop</display-name>
                <type>enum-of-string</type>
                <mandatory-flag>false</mandatory-flag>
                <value-definitions>
                    <value-definition><value>SS25</value></value-definition>
                    <value-definition><value>FW25</value></value-definition>
                </value-definitions>
            </attribute-definition>
        </custom-attribute-definitions>
        <system-attribute-definitions>
            <attribute-definition attribute-id="onlineFlag"/>
        </system-attribute-definitions>
    </type-extension>
    <type-extension type-id="SitePreferences">
        <custom-attribute-definitions>
            <attribute-definition attribute-id="newsletterEnabled">
                <type>boolean</type>
                <mandatory-flag>true</mandatory-flag>
            </attribute-definition>
        </custom-attribute-definitions>
    </type-extension>
</metadata>
"#;

    /// Tests run in parallel, so each gets its own directory.
    fn sample(test: &str) -> Metadata {
        let directory = std::env::temp_dir().join(format!("isml-lsp-metadata-{test}"));
        let _ = fs::create_dir_all(&directory);
        fs::write(directory.join("system-objecttype-extensions.xml"), SAMPLE).unwrap();
        Metadata::scan(&[directory])
    }

    #[test]
    fn reads_custom_attributes_and_leaves_system_ones_out() {
        let metadata = sample("system-out");
        assert!(metadata.attribute("Product", "season").is_some());
        assert!(metadata.attribute("Product", "onlineFlag").is_none());
    }

    #[test]
    fn reads_the_enumerated_values_and_the_type() {
        let metadata = sample("values");
        let definition = metadata.attribute("Product", "season").unwrap();
        assert_eq!(definition.values, ["SS25", "FW25"]);
        assert_eq!(definition.value_type.as_deref(), Some("enum-of-string"));
        assert_eq!(definition.display_name.as_deref(), Some("Drop"));
    }

    #[test]
    fn keeps_site_preferences_as_their_own_type() {
        let metadata = sample("preferences");
        assert!(metadata
            .attribute(SITE_PREFERENCES, "newsletterEnabled")
            .is_some());
        assert!(metadata.is_loaded());
    }

    #[test]
    fn stays_unloaded_when_there_is_no_metadata() {
        let empty = Metadata::scan(&[std::env::temp_dir().join("isml-lsp-no-such-directory")]);
        assert!(!empty.is_loaded());
    }
}
