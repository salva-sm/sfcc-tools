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

/// What one `run` found, for `notify` to send.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Report {
    /// The instance read, or the environment it is.
    pub instance: String,
    /// When the run happened.
    pub generated: String,
    /// The signatures nobody had seen before.
    pub new: Vec<ReportItem>,
    /// Known signatures logged far more today than they used to be.
    #[serde(default)]
    pub spikes: Vec<ReportSpike>,
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
    /// The line it points at, in the repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_url: Option<String>,
}

/// A known signature that spiked.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportSpike {
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
    /// Records today, so far.
    pub today: u64,
    /// Records on an average day of the week before.
    pub usual: f64,
    /// Whether it shows as an error page.
    #[serde(default)]
    pub serious: bool,
    /// The line it points at, in the repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_url: Option<String>,
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

    /// Whether there is nothing to tell anyone.
    pub fn is_empty(&self) -> bool {
        self.new.is_empty() && self.spikes.is_empty()
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

/// Post the report to a Teams channel.
pub async fn teams(webhook: &str, report: &Report) -> Result<()> {
    post(webhook, &card(report)).await
}

/// Post a message to a Teams channel. Works with a Workflows webhook and a
/// legacy incoming webhook alike: both take an Adaptive Card in this envelope.
pub async fn post(webhook: &str, message: &Value) -> Result<()> {
    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?
        .post(webhook)
        .header("Content-Type", "application/json")
        .body(message.to_string())
        .send()
        .await
        .context("cannot reach the webhook")?;
    if !response.status().is_success() {
        bail!("the webhook answered HTTP {}", response.status());
    }
    Ok(())
}

/// An Adaptive Card around `body`, in the envelope Teams webhooks take.
pub fn envelope(body: Vec<Value>, actions: Vec<Value>) -> Value {
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

/// `3 new errors, 1 spike on DEV`.
fn title(report: &Report) -> String {
    let mut parts = Vec::new();
    match report.new.len() {
        0 => {}
        1 => parts.push("1 new error".to_string()),
        count => parts.push(format!("{count} new errors")),
    }
    match report.spikes.len() {
        0 => {}
        1 => parts.push("1 spike".to_string()),
        count => parts.push(format!("{count} spikes")),
    }
    format!("{} on {}", parts.join(", "), report.instance)
}

fn card(report: &Report) -> Value {
    let mut body = vec![json!({
        "type": "TextBlock",
        "size": "Medium",
        "weight": "Bolder",
        "wrap": true,
        "text": title(report),
    })];
    let mut actions: Vec<Value> = Vec::new();
    let mut link = |title: String, url: &str| {
        if !actions.iter().any(|action| action["url"] == url) {
            actions.push(json!({ "type": "Action.OpenUrl", "title": title, "url": url }));
        }
    };

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
            if let Some(url) = &deploy.compare_url {
                link(format!("Commits {range}"), url);
            }
        }
        if let Some(url) = &item.code_url {
            facts.push(json!({ "title": "Code", "value": format!("[{}]({url})", item.location.as_deref().unwrap_or("open")) }));
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
    if report.new.len() > CARD_ITEMS {
        body.push(json!({
            "type": "TextBlock",
            "wrap": true,
            "text": format!("...and {} more in the ledger.", report.new.len() - CARD_ITEMS),
        }));
    }

    if !report.spikes.is_empty() {
        body.push(json!({
            "type": "TextBlock",
            "weight": "Bolder",
            "wrap": true,
            "separator": true,
            "spacing": "Large",
            "text": "Spikes: known errors logged far more than usual",
        }));
    }
    for spike in report.spikes.iter().take(CARD_ITEMS) {
        let what = headline(
            &spike.label,
            spike.exception_class.as_deref(),
            spike.location.as_deref(),
        );
        let mut facts = vec![
            json!({ "title": "Today", "value": format!("x{}", spike.today) }),
            json!({ "title": "Usually", "value": format!("~{:.0} a day", spike.usual) }),
            json!({ "title": "Signature", "value": spike.id }),
        ];
        if let Some(url) = &spike.code_url {
            facts.push(json!({ "title": "Code", "value": format!("[{}]({url})", spike.location.as_deref().unwrap_or("open")) }));
        }
        body.push(json!({
            "type": "TextBlock",
            "wrap": true,
            "text": match spike.serious {
                true => format!("**500** · {what}"),
                false => what,
            },
        }));
        body.push(json!({ "type": "FactSet", "facts": facts }));
    }

    envelope(body, actions)
}

/// A commit, shortened; a code version name, when that is all there is, whole.
fn short(sha: &str) -> &str {
    match sha.len() >= 12 && sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        true => &sha[..9],
        false => sha,
    }
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
            code_url: None,
        }
    }

    fn report(new: Vec<ReportItem>, spikes: Vec<ReportSpike>) -> Report {
        Report {
            instance: "dev01".to_string(),
            generated: String::new(),
            new,
            spikes,
        }
    }

    #[test]
    fn the_card_names_the_failure_and_the_commits_to_suspect() {
        let card = card(&report(
            vec![
                item("a", Some("https://git/compare/1..9")),
                item("b", Some("https://git/compare/1..9")),
            ],
            Vec::new(),
        ));
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
        let body = card(&report(
            (0..CARD_ITEMS + 2)
                .map(|n| item(&n.to_string(), None))
                .collect(),
            Vec::new(),
        ))["attachments"][0]["content"]["body"]
            .clone();
        assert_eq!(
            body.as_array().unwrap().last().unwrap()["text"],
            "...and 2 more in the ledger."
        );
    }

    #[test]
    fn a_spike_says_how_far_above_its_usual_day_it_is() {
        let spike = ReportSpike {
            id: "s".to_string(),
            label: "error".to_string(),
            exception_class: Some("TypeError".to_string()),
            location: None,
            example: String::new(),
            today: 230,
            usual: 12.4,
            serious: true,
            code_url: None,
        };
        let card = card(&report(Vec::new(), vec![spike]));
        let body = &card["attachments"][0]["content"]["body"];

        assert_eq!(body[0]["text"], "1 spike on dev01");
        assert_eq!(body[2]["text"], "**500** · TypeError");
        assert_eq!(body[3]["facts"][1]["value"], "~12 a day");
    }
}
