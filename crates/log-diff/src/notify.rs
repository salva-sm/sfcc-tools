//! Telling someone: the desktop for a developer, a Teams channel for the team,
//! and the report file that sits between `run` and `notify`.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::Path;
use std::time::Duration;

/// Findings listed in one Teams card. The rest are a count and a link.
const CARD_ITEMS: usize = 10;
/// Desktop notifications shown one by one before they become one summary.
const DESKTOP_ITEMS: usize = 3;

/// What one `run` found new, for `notify` to send.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Report {
    /// The instance read.
    pub instance: String,
    /// When the run happened.
    pub generated: String,
    /// The signatures nobody had seen before.
    pub new: Vec<ReportItem>,
}

/// One new signature.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportItem {
    /// The signature id.
    pub id: String,
    /// The level it was logged at.
    pub label: String,
    /// The innermost exception named.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exception_class: Option<String>,
    /// The top script frame.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    /// One occurrence, scrubbed.
    pub example: String,
    /// Occurrences in this run.
    pub count: u64,
    /// The first of them.
    pub first_seen: String,
    /// The deploy that was live when it first showed up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deploy: Option<ReportDeploy>,
}

/// The deploy a new signature is laid at, and the commits it brought.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportDeploy {
    /// The commit deployed.
    pub sha: String,
    /// Its CI build.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<u64>,
    /// The deploy before it; the suspects are the commits in between.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_sha: Option<String>,
    /// A link comparing the two.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compare_url: Option<String>,
}

impl Report {
    /// Read a report `run` wrote.
    pub fn load(path: &Path) -> Result<Report> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        serde_json::from_str(&raw).with_context(|| format!("{} is not a report", path.display()))
    }

    /// Write it for `notify`, or for anything else that wants it.
    pub fn save(&self, path: &Path) -> Result<()> {
        std::fs::write(path, serde_json::to_string_pretty(self)? + "\n")
            .with_context(|| format!("cannot write {}", path.display()))
    }
}

/// A one-line headline for a signature: `TypeError at path/file.js:214`.
pub fn headline(label: &str, exception: Option<&str>, location: Option<&str>) -> String {
    let what = exception.unwrap_or(label);
    match location {
        Some(location) => format!("{what} at {location}"),
        None => what.to_string(),
    }
}

/// Post the report to a Teams channel. Works with a Workflows webhook and a
/// legacy incoming webhook alike: both take an Adaptive Card in this envelope.
pub async fn teams(webhook: &str, report: &Report) -> Result<()> {
    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?
        .post(webhook)
        .header("Content-Type", "application/json")
        .body(card(report).to_string())
        .send()
        .await
        .context("cannot reach the webhook")?;
    if !response.status().is_success() {
        bail!("the webhook answered HTTP {}", response.status());
    }
    Ok(())
}

fn card(report: &Report) -> Value {
    let count = report.new.len();
    let mut body = vec![json!({
        "type": "TextBlock",
        "size": "Medium",
        "weight": "Bolder",
        "wrap": true,
        "text": format!(
            "{count} new error{} on {}",
            if count == 1 { "" } else { "s" },
            report.instance
        ),
    })];
    let mut actions = Vec::new();

    for item in report.new.iter().take(CARD_ITEMS) {
        let mut facts = vec![
            json!({ "title": "Signature", "value": item.id }),
            json!({ "title": "Seen", "value": format!("x{} since {}", item.count, item.first_seen) }),
        ];
        if let Some(deploy) = &item.deploy {
            let build = deploy
                .build
                .map(|build| format!(" (build {build})"))
                .unwrap_or_default();
            let range = match &deploy.previous_sha {
                Some(previous) => format!("{}..{}{build}", short(previous), short(&deploy.sha)),
                None => format!("{}{build}", short(&deploy.sha)),
            };
            facts.push(json!({ "title": "Deploy", "value": range }));
            if let Some(url) = &deploy.compare_url
                && !actions.iter().any(|action: &Value| action["url"] == *url)
            {
                actions.push(json!({
                    "type": "Action.OpenUrl",
                    "title": format!("Commits {}", range),
                    "url": url,
                }));
            }
        }
        body.push(json!({
            "type": "TextBlock",
            "weight": "Bolder",
            "wrap": true,
            "separator": true,
            "text": headline(&item.label, item.exception_class.as_deref(), item.location.as_deref()),
        }));
        body.push(json!({ "type": "FactSet", "facts": facts }));
        body.push(json!({
            "type": "TextBlock",
            "wrap": true,
            "isSubtle": true,
            "fontType": "Monospace",
            "text": item.example,
        }));
    }
    if count > CARD_ITEMS {
        body.push(json!({
            "type": "TextBlock",
            "wrap": true,
            "text": format!("...and {} more in the ledger.", count - CARD_ITEMS),
        }));
    }

    json!({
        "type": "message",
        "attachments": [{
            "contentType": "application/vnd.microsoft.card.adaptive",
            "contentUrl": null,
            "content": {
                "$schema": "http://adaptivecards.io/schemas/adaptive-card.json",
                "type": "AdaptiveCard",
                "version": "1.4",
                "msteams": { "width": "Full" },
                "body": body,
                "actions": actions,
            },
        }],
    })
}

fn short(sha: &str) -> &str {
    sha.get(..9).unwrap_or(sha)
}

/// A desktop notification per headline, or one summary when there are many.
/// Failing to show one is never an error: the terminal already has it.
pub fn desktop(instance: &str, headlines: &[String]) {
    if headlines.is_empty() {
        return;
    }
    let shown: Vec<(String, String)> = match headlines.len() {
        count if count <= DESKTOP_ITEMS => headlines
            .iter()
            .map(|headline| (format!("New SFCC error on {instance}"), headline.clone()))
            .collect(),
        count => vec![(
            format!("{count} new SFCC errors on {instance}"),
            headlines[..DESKTOP_ITEMS].join("\n") + "\n...",
        )],
    };

    for (summary, body) in shown {
        let _ = notify_rust::Notification::new()
            .appname("log-diff")
            .summary(&summary)
            .body(&body)
            .show();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, compare: Option<&str>) -> ReportItem {
        ReportItem {
            id: id.to_string(),
            label: "error".to_string(),
            exception_class: Some("TypeError".to_string()),
            location: Some("app_x/cartridge/scripts/a.js:10".to_string()),
            example: "TypeError: boom".to_string(),
            count: 3,
            first_seen: "2026-09-22T21:38:04Z".to_string(),
            deploy: Some(ReportDeploy {
                sha: "9451cff0123456789".to_string(),
                build: Some(4821),
                previous_sha: Some("1234567abcdef".to_string()),
                compare_url: compare.map(str::to_string),
            }),
        }
    }

    #[test]
    fn the_card_names_the_failure_and_the_commits_to_suspect() {
        let report = Report {
            instance: "dev01".to_string(),
            generated: String::new(),
            new: vec![
                item("a", Some("https://git/compare/1..9")),
                item("b", Some("https://git/compare/1..9")),
            ],
        };
        let card = card(&report);
        let content = &card["attachments"][0]["content"];

        assert_eq!(content["body"][0]["text"], "2 new errors on dev01");
        assert_eq!(
            content["body"][1]["text"],
            "TypeError at app_x/cartridge/scripts/a.js:10"
        );
        assert_eq!(
            content["body"][2]["facts"][2]["value"],
            "1234567ab..9451cff01 (build 4821)"
        );
        // One link per range, however many findings share it.
        assert_eq!(content["actions"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn a_long_report_is_cut_short_on_the_card() {
        let report = Report {
            instance: "dev01".to_string(),
            generated: String::new(),
            new: (0..CARD_ITEMS + 2)
                .map(|n| item(&n.to_string(), None))
                .collect(),
        };
        let body = card(&report)["attachments"][0]["content"]["body"].clone();
        assert_eq!(
            body.as_array().unwrap().last().unwrap()["text"],
            "...and 2 more in the ledger."
        );
    }
}
