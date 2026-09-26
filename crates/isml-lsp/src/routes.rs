//! SFRA route override chains: a `replace` to the left, or a missing `superModule` extend, silently discards handlers.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::cartridgepath::CartridgePath;
use crate::workspace::Cartridge;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    Get,
    Post,
    /// Adds middleware to every request for the route.
    Use,
    /// Adds middleware after what the cartridges to the right defined.
    Append,
    /// Adds middleware before it.
    Prepend,
    /// Discards it and defines the route anew.
    Replace,
}

impl Verb {
    fn parse(name: &str) -> Option<Verb> {
        match name {
            "get" => Some(Verb::Get),
            "post" => Some(Verb::Post),
            "use" => Some(Verb::Use),
            "append" => Some(Verb::Append),
            "prepend" => Some(Verb::Prepend),
            "replace" => Some(Verb::Replace),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Verb::Get => "get",
            Verb::Post => "post",
            Verb::Use => "use",
            Verb::Append => "append",
            Verb::Prepend => "prepend",
            Verb::Replace => "replace",
        }
    }

    fn discards_the_rest(self) -> bool {
        matches!(self, Verb::Replace)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declaration {
    pub route: String,
    pub verb: Verb,
    /// Zero-based.
    pub line: u32,
}

#[derive(Debug, Clone)]
pub struct ControllerFile {
    pub cartridge: String,
    /// The file stem.
    pub controller: String,
    pub path: PathBuf,
    /// False when the file never calls `server.extend(module.superModule)`,
    /// which cuts the chain: nothing to its right is loaded at all.
    pub extends: bool,
    pub declarations: Vec<Declaration>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    Runs,
    /// Reached by no request: replaced, or in a file the chain never loads.
    Shadowed,
    /// In a cartridge this site's path does not contain.
    OutOfPath,
    /// The checkout records no cartridge path, so the order is not knowable.
    Unknown,
}

#[derive(Debug, Clone)]
pub struct Link {
    pub cartridge: String,
    pub verb: Verb,
    pub path: PathBuf,
    /// Zero-based.
    pub line: u32,
    pub effect: Effect,
}

/// Unindexed: the set is small and the questions few.
#[derive(Debug, Default)]
pub struct Controllers {
    files: Vec<ControllerFile>,
}

impl Controllers {
    pub fn scan(cartridges: &[Cartridge]) -> Controllers {
        let mut files = Vec::new();
        for cartridge in cartridges {
            let directory = cartridge.cartridge_dir().join("controllers");
            let Ok(entries) = fs::read_dir(&directory) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_none_or(|extension| extension != "js") {
                    continue;
                }
                if let Some(file) = read_controller(&path, &cartridge.name) {
                    files.push(file);
                }
            }
        }
        Controllers { files }
    }

    pub fn routes_of(&self, controller: &str) -> BTreeSet<&str> {
        self.files
            .iter()
            .filter(|file| file.controller == controller)
            .flat_map(|file| file.declarations.iter().map(|entry| entry.route.as_str()))
            .collect()
    }

    /// Leftmost cartridge first, each link judged by whether a request reaches it.
    pub fn chain(&self, route: &Route, path: Option<&CartridgePath>) -> Vec<Link> {
        let mut ranked: Vec<(Option<usize>, &ControllerFile)> = self
            .files
            .iter()
            .filter(|file| file.controller == route.controller)
            .filter(|file| file.declares(&route.name))
            .map(|file| (path.and_then(|path| path.rank(&file.cartridge)), file))
            .collect();
        ranked.sort_by_key(|(rank, file)| (rank.unwrap_or(usize::MAX), file.cartridge.clone()));

        let mut links = Vec::new();
        let mut loaded = true;
        let mut settled = false;
        for (rank, file) in ranked {
            let mut replaced_here = false;
            for declaration in file
                .declarations
                .iter()
                .filter(|entry| entry.route == route.name)
            {
                let effect = match rank {
                    None if path.is_none() => Effect::Unknown,
                    None => Effect::OutOfPath,
                    Some(_) if !loaded || settled => Effect::Shadowed,
                    Some(_) => Effect::Runs,
                };
                links.push(Link {
                    cartridge: file.cartridge.clone(),
                    verb: declaration.verb,
                    path: file.path.clone(),
                    line: declaration.line,
                    effect,
                });
                replaced_here |= effect == Effect::Runs && declaration.verb.discards_the_rest();
            }
            // A file runs top to bottom, so a `replace` only discards the
            // cartridges to its right — not an `append` further down its own
            // file, which attaches to the route the `replace` just installed.
            settled |= replaced_here;
            if rank.is_some() && !file.extends {
                loaded = false;
            }
        }
        links
    }
}

impl ControllerFile {
    fn declares(&self, route: &str) -> bool {
        self.declarations.iter().any(|entry| entry.route == route)
    }
}

/// A route, as SFRA names it: `Account-Show`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    pub controller: String,
    pub name: String,
}

impl Route {
    pub fn endpoint(&self) -> String {
        format!("{}-{}", self.controller, self.name)
    }
}

fn read_controller(path: &Path, cartridge: &str) -> Option<ControllerFile> {
    let source = fs::read_to_string(path).ok()?;
    let controller = path.file_stem()?.to_str()?.to_string();
    Some(ControllerFile {
        cartridge: cartridge.to_string(),
        controller,
        path: path.to_path_buf(),
        extends: extends_super_module(&source),
        declarations: declarations(&source),
    })
}

/// Whether a controller chains to the one to its right; a file that does not cuts the chain.
/// Covers `server.extend(module.superModule)` and the more common `var page = module.superModule; server.extend(page);`.
pub fn extends_super_module(source: &str) -> bool {
    source.contains("server.extend(") && source.contains("module.superModule")
}

/// Scans the whole source, not line by line: the route often sits on the line below the call.
pub fn declarations(source: &str) -> Vec<Declaration> {
    const CALL: &str = "server.";
    let mut found = Vec::new();
    let mut cursor = 0;
    while let Some(offset) = source[cursor..].find(CALL) {
        let start = cursor + offset;
        cursor = start + CALL.len();
        if is_commented_out(source, start) {
            continue;
        }
        let rest = &source[cursor..];
        let Some(open) = rest.find('(') else {
            continue;
        };
        let Some(verb) = Verb::parse(rest[..open].trim()) else {
            continue;
        };
        let Some(route) = first_literal(&rest[open + 1..]) else {
            continue;
        };
        found.push(Declaration {
            route,
            verb,
            line: source[..start].matches('\n').count() as u32,
        });
    }
    found
}

fn is_commented_out(source: &str, offset: usize) -> bool {
    let line_start = source[..offset].rfind('\n').map_or(0, |index| index + 1);
    let head = source[line_start..offset].trim_start();
    head.starts_with("//") || head.starts_with('*')
}

fn first_literal(text: &str) -> Option<String> {
    let trimmed = text.trim_start();
    let quote = trimmed.chars().next()?;
    if quote != '\'' && quote != '"' {
        return None;
    }
    let body = &trimmed[quote.len_utf8()..];
    let end = body.find(quote)?;
    let route = &body[..end];
    route
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_')
        .then(|| route.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = r#"
var server = require('server');
server.get('Show', cache.applyDefaultCache, function (req, res, next) {});
server.post('Login', function (req, res, next) {});
"#;

    const BRAND: &str = r#"
var server = require('server');
server.extend(module.superModule);
// server.get('Ignored', function () {});
server.append('Show', function (req, res, next) {});
"#;

    fn controller(cartridge: &str, source: &str, extends: bool) -> ControllerFile {
        ControllerFile {
            cartridge: cartridge.to_string(),
            controller: "Account".to_string(),
            path: PathBuf::from(format!("{cartridge}/Account.js")),
            extends,
            declarations: declarations(source),
        }
    }

    fn path_of(order: &[&str]) -> CartridgePath {
        CartridgePath {
            label: "storefront".into(),
            order: order.iter().map(|name| name.to_string()).collect(),
        }
    }

    fn route() -> Route {
        Route {
            controller: "Account".into(),
            name: "Show".into(),
        }
    }

    #[test]
    fn reads_the_verb_and_the_route_off_a_declaration() {
        let found = declarations(BASE);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].route, "Show");
        assert_eq!(found[0].verb, Verb::Get);
        assert_eq!(found[1].verb, Verb::Post);
    }

    #[test]
    fn reads_a_declaration_whose_route_is_on_the_next_line() {
        let source =
            "server.get(\n    'Show',\n    server.middleware.https,\n    function () {}\n);";
        let found = declarations(source);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].route, "Show");
        assert_eq!(found[0].verb, Verb::Get);
        assert_eq!(found[0].line, 0);
    }

    #[test]
    fn accepts_both_ways_of_extending_the_super_module() {
        assert!(extends_super_module("server.extend(module.superModule);"));
        assert!(extends_super_module(
            "var page = module.superModule;\nserver.extend(page);"
        ));
        assert!(!extends_super_module("server.get('Show', function () {});"));
    }

    #[test]
    fn leaves_a_commented_out_declaration_alone() {
        let found = declarations(BRAND);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].verb, Verb::Append);
    }

    #[test]
    fn orders_the_chain_by_the_cartridge_path() {
        let controllers = Controllers {
            files: vec![
                controller("app_storefront_base", BASE, false),
                controller("app_brand", BRAND, true),
            ],
        };
        let chain = controllers.chain(
            &route(),
            Some(&path_of(&["app_brand", "app_storefront_base"])),
        );
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[0].cartridge, "app_brand");
        assert_eq!(chain[0].verb, Verb::Append);
        assert_eq!(chain[1].cartridge, "app_storefront_base");
        assert!(chain.iter().all(|link| link.effect == Effect::Runs));
    }

    #[test]
    fn marks_what_a_replace_to_the_left_discards() {
        let replacing =
            "server.extend(module.superModule);\nserver.replace('Show', function () {});";
        let controllers = Controllers {
            files: vec![
                controller("app_storefront_base", BASE, false),
                controller("app_brand", BRAND, true),
                controller("plugin_overrides", replacing, true),
            ],
        };
        let chain = controllers.chain(
            &route(),
            Some(&path_of(&[
                "plugin_overrides",
                "app_brand",
                "app_storefront_base",
            ])),
        );
        assert_eq!(chain[0].effect, Effect::Runs);
        assert_eq!(chain[0].verb, Verb::Replace);
        assert_eq!(chain[1].effect, Effect::Shadowed);
        assert_eq!(chain[2].effect, Effect::Shadowed);
    }

    #[test]
    fn keeps_an_append_that_follows_a_replace_in_the_same_file() {
        let both = "server.extend(module.superModule);\n\
                    server.replace('Show', function () {});\n\
                    server.append('Show', function () {});";
        let controllers = Controllers {
            files: vec![
                controller("app_storefront_base", BASE, false),
                controller("app_brand", both, true),
            ],
        };
        let chain = controllers.chain(
            &route(),
            Some(&path_of(&["app_brand", "app_storefront_base"])),
        );
        assert_eq!(chain[0].verb, Verb::Replace);
        assert_eq!(chain[0].effect, Effect::Runs);
        assert_eq!(chain[1].verb, Verb::Append);
        assert_eq!(chain[1].effect, Effect::Runs);
        assert_eq!(chain[2].cartridge, "app_storefront_base");
        assert_eq!(chain[2].effect, Effect::Shadowed);
    }

    #[test]
    fn cuts_the_chain_where_a_controller_does_not_extend_its_super_module() {
        let standalone = "server.get('Show', function () {});";
        let controllers = Controllers {
            files: vec![
                controller("app_storefront_base", BASE, false),
                controller("app_brand", standalone, false),
            ],
        };
        let chain = controllers.chain(
            &route(),
            Some(&path_of(&["app_brand", "app_storefront_base"])),
        );
        assert_eq!(chain[0].effect, Effect::Runs);
        assert_eq!(chain[1].effect, Effect::Shadowed);
    }

    #[test]
    fn keeps_a_cartridge_the_site_does_not_use_out_of_the_running() {
        let controllers = Controllers {
            files: vec![controller("app_other_brand", BRAND, true)],
        };
        let chain = controllers.chain(&route(), Some(&path_of(&["app_brand"])));
        assert_eq!(chain[0].effect, Effect::OutOfPath);
    }
}
