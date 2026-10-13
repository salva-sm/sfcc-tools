//! Route hover: who declares it, in what order, and which one a request reaches.

use std::path::{Path, PathBuf};

use lsp_types::Url;

use crate::api;
use crate::reference::Reference;
use crate::resolve::Override;
use crate::routes::{Effect, Link, Route};
use crate::workspace::Workspace;

/// A bare route name in `server.<verb>` belongs to the controller being edited.
pub fn route_of(reference: &Reference, file: &Path) -> Option<Route> {
    let Reference::Route { controller, name } = reference else {
        return None;
    };
    let controller = match controller {
        Some(named) => named.clone(),
        None => file.file_stem()?.to_str()?.to_string(),
    };
    Some(Route {
        controller,
        name: name.clone(),
    })
}

pub fn member_markdown(line: &str, column: usize, text: &str) -> Option<String> {
    let (class, name) = api::member_at(line, column, text)?;
    api_member_markdown(&class, &name)
}

/// A member of a `dw.*` class: its shape, description and kind.
pub fn api_member_markdown(class: &str, name: &str) -> Option<String> {
    let (member, kind) = api::api().member(class, name)?;

    // The signature carries its own `static`, so the class goes on its own
    // line rather than in front of a modifier.
    let shape = match member.shape.is_empty() {
        true => name.to_string(),
        false => member.shape.clone(),
    };
    let mut out = format!("```js\n{class}\n{shape}\n```\n");
    if !member.description.is_empty() {
        out.push_str(&format!("\n{}\n", member.description));
    }
    out.push_str(&format!("\n`{}`\n", kind_label(kind)));
    Some(out)
}

pub fn module_markdown(reference: &Reference) -> Option<String> {
    let Reference::Module(path) = reference else {
        return None;
    };
    let class = api::api().by_module(path)?;
    let qualified = path.trim_matches('/').replace('/', ".");
    let mut out = format!("**{qualified}**\n");
    if !class.description.is_empty() {
        out.push_str(&format!("\n{}\n", class.description));
    }
    out.push_str(&format!(
        "\n{} methods, {} properties, {} constants\n",
        class.methods.len(),
        class.properties.len(),
        class.constants.len()
    ));
    Some(out)
}

/// Which copy of a `*/` module or a template each storefront runs. Nothing without a
/// recorded cartridge path: the order would be a guess, and `Go to Definition` lists them.
pub fn overrides_markdown(
    title: &str,
    overrides: &[Override],
    workspace: &Workspace,
    current: &Path,
) -> Option<String> {
    if overrides.is_empty() || workspace.paths.is_empty() {
        return None;
    }
    // A template can exist in several locale folders of one cartridge; the
    // path chooses between cartridges, so each is listed once.
    let mut copies: Vec<&Override> = Vec::new();
    for candidate in overrides {
        if !copies
            .iter()
            .any(|seen| seen.cartridge == candidate.cartridge)
        {
            copies.push(candidate);
        }
    }

    let mut out = format!("**`{title}`**\n");
    for path in &workspace.paths {
        let mut ranked: Vec<(Option<usize>, &Override)> = copies
            .iter()
            .map(|candidate| (path.rank(&candidate.cartridge), *candidate))
            .collect();
        ranked.sort_by_key(|(rank, _)| rank.unwrap_or(usize::MAX));

        out.push_str(&format!("\n`{}`\n", path.label));
        out.push_str("\n| | Cartridge | |\n| --- | --- | --- |\n");
        for (index, (rank, candidate)) in ranked.iter().enumerate() {
            let effect = match rank {
                None => "not in this path",
                Some(_) if index == 0 => "runs",
                Some(_) => "overridden",
            };
            let here = match candidate.hit.path == current {
                true => " ← this file",
                false => "",
            };
            out.push_str(&format!(
                "| {} | {} | {}{} |\n",
                index + 1,
                linked(&candidate.cartridge, &candidate.hit.path),
                effect,
                here
            ));
        }
    }
    Some(out)
}

/// The cartridge name, linked to its copy so the hover opens it.
fn linked(cartridge: &str, file: &Path) -> String {
    match Url::from_file_path(file) {
        Ok(url) => format!("[`{cartridge}`]({url})"),
        Err(()) => format!("`{cartridge}`"),
    }
}

fn kind_label(kind: api::MemberKind) -> &'static str {
    match kind {
        api::MemberKind::Method => "method",
        api::MemberKind::Property => "property",
        api::MemberKind::Constant => "constant",
    }
}

pub struct Chain {
    pub label: Option<String>,
    /// Leftmost cartridge first.
    pub links: Vec<Link>,
}

/// One chain per recorded cartridge path; a single unordered one when none is recorded.
pub fn chains(route: &Route, workspace: &Workspace) -> Vec<Chain> {
    let controllers = workspace.controllers();
    if workspace.paths.is_empty() {
        let links = controllers.chain(route, None);
        return match links.is_empty() {
            true => Vec::new(),
            false => vec![Chain { label: None, links }],
        };
    }
    workspace
        .paths
        .iter()
        .map(|path| Chain {
            label: Some(path.label.clone()),
            links: controllers.chain(route, Some(path)),
        })
        .filter(|chain| !chain.links.is_empty())
        .collect()
}

/// Every declaration, most relevant first, for `Go to Definition`.
pub fn targets(chains: &[Chain]) -> Vec<(PathBuf, u32)> {
    let mut targets: Vec<(PathBuf, u32)> = Vec::new();
    for effect in [
        Effect::Runs,
        Effect::Unknown,
        Effect::Shadowed,
        Effect::OutOfPath,
    ] {
        for chain in chains {
            for link in chain.links.iter().filter(|link| link.effect == effect) {
                let target = (link.path.clone(), link.line);
                if !targets.contains(&target) {
                    targets.push(target);
                }
            }
        }
    }
    targets
}

pub fn markdown(route: &Route, chains: &[Chain], current: &Path) -> Option<String> {
    if chains.is_empty() {
        return None;
    }
    let mut out = format!("**{}**\n", route.endpoint());

    for chain in chains {
        if let Some(label) = &chain.label {
            out.push_str(&format!("\n`{label}`\n"));
        }
        out.push_str("\n| | Cartridge | | |\n| --- | --- | --- | --- |\n");
        for (index, link) in chain.links.iter().enumerate() {
            let here = match link.path == current {
                true => " ← this file",
                false => "",
            };
            out.push_str(&format!(
                "| {} | `{}` | {} | {}{} |\n",
                index + 1,
                link.cartridge,
                link.verb.label(),
                note(link.effect),
                here
            ));
        }
        if let Some(warning) = warning(&chain.links) {
            out.push_str(&format!("\n{warning}\n"));
        }
    }

    if chains.iter().all(|chain| chain.label.is_none()) {
        out.push_str(
            "\nOrder unknown: nothing here records a cartridge path. Set `cartridge_path` in \
             this server's initialization options, add a `cartridge` array to `dw.json`, or \
             check a site archive in.\n",
        );
    }
    Some(out)
}

fn note(effect: Effect) -> &'static str {
    match effect {
        Effect::Runs => "runs",
        Effect::Shadowed => "**never reached**",
        Effect::OutOfPath => "not in this path",
        Effect::Unknown => "",
    }
}

/// The sentence worth reading when something in the chain is dead.
fn warning(links: &[Link]) -> Option<String> {
    let shadowing = links
        .iter()
        .take_while(|link| link.effect == Effect::Runs)
        .last()?;
    let shadowed = links.iter().find(|link| link.effect == Effect::Shadowed)?;
    Some(format!(
        "`{}` {}s this route, so `{}` never runs.",
        shadowing.cartridge,
        shadowing.verb.label(),
        shadowed.cartridge
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::Verb;

    fn link(cartridge: &str, verb: Verb, effect: Effect) -> Link {
        Link {
            cartridge: cartridge.into(),
            verb,
            path: PathBuf::from(format!("{cartridge}/Account.js")),
            line: 3,
            effect,
        }
    }

    fn route() -> Route {
        Route {
            controller: "Account".into(),
            name: "Show".into(),
        }
    }

    #[test]
    fn names_the_endpoint_and_every_link() {
        let chains = vec![Chain {
            label: Some("storefront".into()),
            links: vec![
                link("app_brand", Verb::Append, Effect::Runs),
                link("app_storefront_base", Verb::Get, Effect::Runs),
            ],
        }];
        let text = markdown(&route(), &chains, Path::new("nowhere")).unwrap();
        assert!(text.contains("**Account-Show**"));
        assert!(text.contains("`storefront`"));
        assert!(text.contains("app_storefront_base"));
        assert!(!text.contains("never reached"));
    }

    #[test]
    fn says_out_loud_which_cartridge_kills_which() {
        let chains = vec![Chain {
            label: Some("storefront".into()),
            links: vec![
                link("plugin_overrides", Verb::Replace, Effect::Runs),
                link("app_brand", Verb::Append, Effect::Shadowed),
            ],
        }];
        let text = markdown(&route(), &chains, Path::new("nowhere")).unwrap();
        assert!(text.contains("`plugin_overrides` replaces this route, so `app_brand` never runs."));
    }

    #[test]
    fn marks_the_file_being_read() {
        let chains = vec![Chain {
            label: None,
            links: vec![link("app_brand", Verb::Append, Effect::Unknown)],
        }];
        let here = PathBuf::from("app_brand/Account.js");
        let text = markdown(&route(), &chains, &here).unwrap();
        assert!(text.contains("← this file"));
        assert!(text.contains("nothing here records a cartridge path"));
    }

    fn copy(cartridge: &str) -> Override {
        Override {
            cartridge: cartridge.into(),
            hit: copy_path(cartridge).into(),
        }
    }

    fn copy_path(cartridge: &str) -> PathBuf {
        std::env::temp_dir()
            .join(cartridge)
            .join("productHelpers.js")
    }

    fn linked_copy(cartridge: &str) -> String {
        let url = Url::from_file_path(copy_path(cartridge)).unwrap();
        format!("[`{cartridge}`]({url})")
    }

    #[test]
    fn says_which_copy_each_storefront_runs() {
        let settings = serde_json::json!({
            "cartridge_path": {
                "site_a": "app_na:app_brand:app_storefront_base",
                "site_b": "app_brand:app_storefront_base",
            }
        });
        let workspace = Workspace::scan(&[], &settings);
        let copies = [
            copy("app_brand"),
            copy("app_na"),
            copy("app_storefront_base"),
        ];
        let here = copy_path("app_brand");
        let text = overrides_markdown("*/cartridge/x", &copies, &workspace, &here).unwrap();

        let (na, brand) = (linked_copy("app_na"), linked_copy("app_brand"));
        let site_a = text.split("`site_b`").next().unwrap();
        assert!(site_a.contains(&format!("| 1 | {na} | runs |")));
        assert!(site_a.contains(&format!("| 2 | {brand} | overridden ← this file |")));
        let site_b = text.split("`site_b`").nth(1).unwrap();
        assert!(site_b.contains(&format!("| 1 | {brand} | runs ← this file |")));
        assert!(site_b.contains(&format!("| 3 | {na} | not in this path |")));
    }

    #[test]
    fn says_nothing_about_overrides_without_a_cartridge_path() {
        let workspace = Workspace::scan(&[], &serde_json::Value::Null);
        let copies = [copy("app_brand")];
        assert!(overrides_markdown("*/cartridge/x", &copies, &workspace, Path::new("x")).is_none());
    }

    #[test]
    fn puts_what_runs_before_what_does_not_when_jumping() {
        let chains = vec![Chain {
            label: Some("storefront".into()),
            links: vec![
                link("a", Verb::Append, Effect::Shadowed),
                link("b", Verb::Get, Effect::Runs),
            ],
        }];
        let targets = targets(&chains);
        assert_eq!(targets[0].0, PathBuf::from("b/Account.js"));
        assert_eq!(targets[1].0, PathBuf::from("a/Account.js"));
    }
}
