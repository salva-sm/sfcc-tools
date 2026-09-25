//! One HTML file that shows what the ledgers know: how much is logged, what
//! is new, what spiked, what hurts most and what reached production - with
//! charts and rankings, across environments.
//!
//! The page is self-contained: the data is embedded, the scripts are inline,
//! nothing is fetched. It can be opened from a checkout of the ledger
//! repository, attached to a workflow run, or served from Pages, and it never
//! shows more than the ledgers hold - scrubbed signatures, no raw log.

use crate::envs::Environment;
use crate::ledger::serious;
use crate::output::{controller, message};
use crate::team::Team;
use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use serde_json::{Map, Value, json};
use std::path::Path;

const TEMPLATE: &str = include_str!("dashboard.html");
const PLACEHOLDER: &str = "__LOG_DIFF_DATA__";

/// Links the page can build, with the same placeholders as `run`'s.
pub struct Links {
    /// A line of code: `{sha}`, `{path}`, `{line}`.
    pub code: Option<String>,
    /// Two deploys compared: `{from}`, `{to}`.
    pub compare: Option<String>,
}

/// The data the page reads.
pub fn data(environments: &[Environment], team: &Team, links: &Links) -> Value {
    let environments: Vec<Value> = environments
        .iter()
        .map(|environment| {
            let ledger = &environment.ledger;
            let mut signatures = Map::new();
            for (id, known) in &ledger.known_signatures {
                let head = known.example.lines().next().unwrap_or_default();
                signatures.insert(
                    id.clone(),
                    json!({
                        "label": known.label,
                        "exception": known.exception_class,
                        "location": known.location,
                        "message": message(head, known.exception_class.as_deref()),
                        "controller": controller(head),
                        "example": known.example,
                        "first": known.first_seen,
                        "last": known.last_seen,
                        "count": known.count,
                        "deploy": known.first_deploy_sha,
                        "serious": serious(&known.label, &known.example),
                        "spiked": known.spiked_on,
                    }),
                );
            }
            let deploys: Vec<Value> = ledger
                .deploy_log
                .iter()
                .map(|deploy| {
                    json!({
                        "sha": deploy.sha,
                        "build": deploy.build,
                        "at": deploy.timestamp,
                        "new": deploy.new_signatures.len(),
                    })
                })
                .collect();
            json!({
                "name": environment.name,
                "instance": ledger.instance,
                "updated": ledger.cursor.as_ref().map(|cursor| cursor.taken.clone()),
                "deploys": deploys,
                "signatures": signatures,
                "daily": ledger.daily,
            })
        })
        .collect();

    json!({
        "generated": Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        "today": Utc::now().format("%Y-%m-%d").to_string(),
        "links": { "code": links.code, "compare": links.compare },
        "team": { "muted": team.muted, "tickets": team.tickets },
        "environments": environments,
    })
}

/// The page with `data` in it. The data sits in a JSON script block, where
/// the only thing that could end it early is `</`, so that is escaped.
pub fn page(data: &Value) -> String {
    let json = data.to_string().replace("</", "<\\/");
    TEMPLATE.replace(PLACEHOLDER, &json)
}

/// Write the page.
pub fn write(path: &Path, data: &Value) -> Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    std::fs::write(path, page(data)).with_context(|| format!("cannot write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::Ledger;

    #[test]
    fn the_data_cannot_close_the_script_block_it_sits_in() {
        let page = page(&json!({ "example": "</script><script>alert(1)</script>" }));
        let block = page.split("id=\"log-diff-data\"").nth(1).unwrap();
        let inside = &block[..block.find("</script>").unwrap()];
        assert!(inside.contains("<\\/script>"));
        assert!(!page.contains(PLACEHOLDER));
    }

    #[test]
    fn every_environment_is_there_even_one_not_read_yet() {
        let environments = vec![
            Environment {
                name: "dev".into(),
                ledger: Ledger::default(),
            },
            Environment {
                name: "prd".into(),
                ledger: Ledger::default(),
            },
        ];
        let data = data(
            &environments,
            &Team::default(),
            &Links {
                code: None,
                compare: None,
            },
        );
        assert_eq!(data["environments"].as_array().unwrap().len(), 2);
        assert_eq!(data["environments"][1]["name"], "prd");
        assert!(data["environments"][1]["updated"].is_null());
    }
}
