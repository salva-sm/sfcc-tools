//! The override chain of a route, as something to read.
//!
//! Hovering a route name answers the question the file cannot: who else
//! declares it, in what order, and which of those a request actually reaches.

use std::path::{Path, PathBuf};

use crate::api;
use crate::reference::Reference;
use crate::routes::{Effect, Link, Route};
use crate::workspace::Workspace;

/// The route a reference names. A bare route name in a `server.<verb>` call
/// belongs to the controller being edited, so the file supplies the half the
/// literal leaves out.
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

/// `Site.getCurrent` — the signature and what the platform says it does.
pub fn member_markdown(line: &str, column: usize, text: &str) -> Option<String> {
    let (class, name) = api::member_at(line, column, text)?;
    let (member, kind) = api::api().class(&class)?.member(&name)?;

    // The signature carries its own `static`, so the class goes on its own
    // line rather than in front of a modifier.
    let shape = match member.shape.is_empty() {
        true => name.clone(),
        false => member.shape.clone(),
    };
    let mut out = format!("```js\n{class}\n{shape}\n```\n");
    if !member.description.is_empty() {
        out.push_str(&format!("\n{}\n", member.description));
    }
    out.push_str(&format!("\n`{}`\n", kind_label(kind)));
    Some(out)
}

/// `require('dw/system/Site')` — what that class is for.
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

fn kind_label(kind: api::MemberKind) -> &'static str {
    match kind {
        api::MemberKind::Method => "method",
        api::MemberKind::Property => "property",
        api::MemberKind::Constant => "constant",
    }
}

/// One cartridge path's answer, labelled by the site it belongs to.
pub struct Chain {
    /// The site this order belongs to, or `None` when none is known.
    pub label: Option<String>,
    /// The declarations, leftmost cartridge first.
    pub links: Vec<Link>,
}

/// The chain under every cartridge path the checkout records; one unordered
/// chain when it records none.
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

/// The chain as a table per site, with the file being read marked.
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
