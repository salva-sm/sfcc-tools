use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::Path;
use std::time::Duration;

const CARD_ITEMS: usize = 10;
/// Past this many new signatures in one run, they are one event (a deploy that broke everything,
/// logs signed a new way), not a list of failures.
pub const FLOOD: usize = 30;
const FLOOD_LINES: usize = 15;
const CARD_EXAMPLE_CHARS: usize = 300;
/// Teams refuses a message past about 28 KB, saying so only with an error; this leaves room
/// for the envelope's escaping.
pub const CARD_BYTES: usize = 24_000;
const DESKTOP_ITEMS: usize = 3;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Report {
    /// The instance read, or the environment it is.
    pub instance: String,
    pub generated: String,
    pub new: Vec<ReportItem>,
    #[serde(default)]
    pub spikes: Vec<ReportSpike>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportItem {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exception_class: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    pub example: String,
    pub count: u64,
    pub first_seen: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deploy: Option<ReportDeploy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportSpike {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exception_class: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    pub example: String,
    pub today: u64,
    /// Records on an average day of the week before.
    pub usual: f64,
    #[serde(default)]
    pub serious: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportDeploy {
    pub sha: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<u64>,
    /// The suspects are the commits between this and `sha`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compare_url: Option<String>,
}

impl Report {
    pub fn load(path: &Path) -> Result<Report> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        serde_json::from_str(&raw).with_context(|| format!("{} is not a report", path.display()))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        std::fs::write(path, serde_json::to_string_pretty(self)? + "\n")
            .with_context(|| format!("cannot write {}", path.display()))
    }

    pub fn is_empty(&self) -> bool {
        self.new.is_empty() && self.spikes.is_empty()
    }
}

/// `TypeError at path/file.js:214`.
pub fn headline(label: &str, exception: Option<&str>, location: Option<&str>) -> String {
    let what = exception.unwrap_or(label);
    match location {
        Some(location) => format!("{what} at {location}"),
        None => what.to_string(),
    }
}

pub async fn teams(webhook: &str, report: &Report) -> Result<()> {
    post(webhook, &card(report)).await
}

/// Workflows and legacy incoming webhooks alike take an Adaptive Card in [`envelope`].
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

#[derive(Clone, Copy)]
struct Shape {
    items: usize,
    examples: bool,
}

/// As much of the report as fits, a flood as one event.
fn card(report: &Report) -> Value {
    if report.new.len() > FLOOD {
        return flood(report);
    }
    let shapes = [
        Shape {
            items: CARD_ITEMS,
            examples: true,
        },
        Shape {
            items: CARD_ITEMS,
            examples: false,
        },
        Shape {
            items: 3,
            examples: false,
        },
        Shape {
            items: 0,
            examples: false,
        },
    ];
    let mut card = Value::Null;
    for shape in shapes {
        card = shaped(report, shape);
        if weight(&card) <= CARD_BYTES {
            break;
        }
    }
    card
}

fn weight(message: &Value) -> usize {
    message.to_string().len()
}

fn shaped(report: &Report, shape: Shape) -> Value {
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

    for item in report.new.iter().take(shape.items) {
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
        if shape.examples {
            body.push(json!({
                "type": "TextBlock",
                "wrap": true,
                "isSubtle": true,
                "fontType": "Monospace",
                "text": clip(&item.example, CARD_EXAMPLE_CHARS),
            }));
        }
    }
    if report.new.len() > shape.items {
        body.push(json!({
            "type": "TextBlock",
            "wrap": true,
            "text": match shape.items {
                0 => format!("{} new, listed in the ledger.", report.new.len()),
                shown => format!("...and {} more in the ledger.", report.new.len() - shown),
            },
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
    for spike in report.spikes.iter().take(shape.items) {
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
    if !report.spikes.is_empty() && report.spikes.len() > shape.items {
        body.push(json!({
            "type": "TextBlock",
            "wrap": true,
            "text": format!("...and {} more spiking.", report.spikes.len() - shape.items),
        }));
    }

    envelope(body, actions)
}

fn flood(report: &Report) -> Value {
    let mut body = vec![
        json!({
            "type": "TextBlock",
            "size": "Medium",
            "weight": "Bolder",
            "wrap": true,
            "text": title(report),
        }),
        json!({
            "type": "TextBlock",
            "wrap": true,
            "text": format!(
                "More than {FLOOD} at once: something broke broadly - a deploy, a service it depends on - rather than {} separate failures. The most logged:",
                report.new.len()
            ),
        }),
    ];
    let mut items: Vec<&ReportItem> = report.new.iter().collect();
    items.sort_by_key(|item| std::cmp::Reverse(item.count));
    let lines: Vec<String> = items
        .iter()
        .take(FLOOD_LINES)
        .map(|item| {
            format!(
                "- x{} {}",
                item.count,
                clip(
                    &headline(
                        &item.label,
                        item.exception_class.as_deref(),
                        item.location.as_deref()
                    ),
                    160
                )
            )
        })
        .collect();
    body.push(json!({ "type": "TextBlock", "wrap": true, "text": lines.join("\n") }));

    let mut actions = Vec::new();
    // The deploy the most logged came with is the one to look at.
    if let Some(deploy) = items.iter().find_map(|item| item.deploy.as_ref())
        && let Some(url) = &deploy.compare_url
    {
        actions.push(json!({ "type": "Action.OpenUrl", "title": format!("Commits of {}", short(&deploy.sha)), "url": url }));
    }
    if !report.spikes.is_empty() {
        body.push(json!({
            "type": "TextBlock",
            "wrap": true,
            "text": format!("Also {} known errors spiking.", report.spikes.len()),
        }));
    }
    envelope(body, actions)
}

fn clip(text: &str, chars: usize) -> String {
    match text.char_indices().nth(chars) {
        Some((end, _)) => format!("{}...", &text[..end]),
        None => text.to_string(),
    }
}

/// A code version name, when that is all there is, is kept whole.
fn short(sha: &str) -> &str {
    match sha.len() >= 12 && sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        true => &sha[..9],
        false => sha,
    }
}

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
    fn a_flood_is_one_event_with_its_most_logged() {
        let new: Vec<ReportItem> = (0..FLOOD as u64 + 5)
            .map(|n| ReportItem {
                count: n,
                ..item(&n.to_string(), Some("https://git/compare/1..9"))
            })
            .collect();
        let card = card(&report(new, Vec::new()));
        let content = &card["attachments"][0]["content"];
        let body = content["body"].as_array().unwrap();

        assert_eq!(body[0]["text"], "35 new errors on dev01");
        let lines = body[2]["text"].as_str().unwrap();
        assert_eq!(lines.lines().count(), FLOOD_LINES);
        assert!(lines.starts_with("- x34 TypeError"));
        assert_eq!(content["actions"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn a_card_too_heavy_for_teams_drops_what_it_can_do_without() {
        let bulky = "x".repeat(4000);
        let new: Vec<ReportItem> = (0..CARD_ITEMS)
            .map(|n| ReportItem {
                example: bulky.clone(),
                location: Some(format!("{bulky}/{n}.js:1")),
                ..item(&n.to_string(), None)
            })
            .collect();
        let card = card(&report(new, Vec::new()));

        assert!(weight(&card) <= CARD_BYTES, "{} bytes", weight(&card));
        assert_eq!(
            card["attachments"][0]["content"]["body"][0]["text"],
            "10 new errors on dev01"
        );
    }

    #[test]
    fn an_example_is_cut_on_the_card() {
        let new = vec![ReportItem {
            example: "é".repeat(CARD_EXAMPLE_CHARS * 2),
            ..item("a", None)
        }];
        let card = card(&report(new, Vec::new()));
        let example = card["attachments"][0]["content"]["body"][3]["text"]
            .as_str()
            .unwrap();
        assert_eq!(example.chars().count(), CARD_EXAMPLE_CHARS + 3);
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
