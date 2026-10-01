use crate::ledger::Known;
use crate::notify::headline;
use crate::team::Ticket;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::time::Duration;

/// Jira Cloud: basic auth with an account's email and API token.
pub struct Target {
    pub url: String,
    pub email: String,
    pub token: String,
    pub project: String,
    pub kind: String,
}

impl Target {
    pub fn from_env(project: String, kind: String) -> Result<Target> {
        let var = |name: &str| {
            std::env::var(name)
                .ok()
                .filter(|value| !value.trim().is_empty())
                .with_context(|| format!("{name} is not set"))
        };
        Ok(Target {
            url: var("JIRA_URL")?.trim_end_matches('/').to_string(),
            email: var("JIRA_EMAIL")?,
            token: var("JIRA_API_TOKEN")?,
            project,
            kind,
        })
    }
}

/// The description is in Atlassian Document Format.
pub fn issue(
    target: &Target,
    id: &str,
    environment: &str,
    known: &Known,
    link: Option<&str>,
    dashboard: Option<&str>,
) -> Value {
    let what = headline(
        &known.label,
        known.exception_class.as_deref(),
        known.location.as_deref(),
    );
    let paragraph = |text: String| json!({ "type": "paragraph", "content": [{ "type": "text", "text": text }] });
    let mut content = vec![
        paragraph(format!(
            "Logged {} times on {} between {} and {} ({}).",
            known.count, environment, known.first_seen, known.last_seen, known.label
        )),
        paragraph(format!("Signature {id}, from log-diff.")),
    ];
    if let Some(sha) = &known.first_deploy_sha {
        content.push(paragraph(format!("First seen after deploy {sha}.")));
    }
    // Where it happened: one signature spans every site and controller it reached.
    let most = |counts: &std::collections::BTreeMap<String, u64>| {
        let mut all: Vec<_> = counts.iter().collect();
        all.sort_by(|left, right| right.1.cmp(left.1));
        all.iter()
            .take(6)
            .map(|(name, count)| format!("{name} {count}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    if !known.sites.is_empty() {
        content.push(paragraph(format!("Sites: {}.", most(&known.sites))));
    }
    if !known.controllers.is_empty() {
        content.push(paragraph(format!(
            "Controllers: {}.",
            most(&known.controllers)
        )));
    }
    let linked = |text: &str, href: &str| {
        json!({ "type": "paragraph", "content": [{
            "type": "text", "text": text, "marks": [{ "type": "link", "attrs": { "href": href } }]
        }]})
    };
    if let Some(link) = link {
        content.push(linked("Code", link));
    }
    if let Some(dashboard) = dashboard {
        content.push(linked("In the dashboard", dashboard));
    }
    content.push(
        json!({ "type": "codeBlock", "content": [{ "type": "text", "text": known.example }] }),
    );

    json!({ "fields": {
        "project": { "key": target.project },
        "issuetype": { "name": target.kind },
        "summary": summary(environment, &what, &known.example),
        "labels": ["log-diff", environment],
        "description": { "type": "doc", "version": 1, "content": content },
    }})
}

/// `[PRD] TypeError at a.js:9: Cannot read property "x" from null`: what, where, and the
/// message, which tells two warnings of one kind apart.
fn summary(environment: &str, what: &str, example: &str) -> String {
    let head = example.lines().next().unwrap_or_default();
    // After a custom log's `[]`, a system log's counter, or its six empty columns.
    let message = match (
        head.find("[] "),
        head.find(" <n> - "),
        head.find(" - - - - - - "),
    ) {
        (Some(at), _, _) => &head[at + 3..],
        (None, Some(at), _) => &head[at + 7..],
        (None, None, Some(at)) => &head[at + 13..],
        _ => "",
    };
    // The instance's filter repeating it says so first.
    let message = message
        .split_once(" seconds: ")
        .filter(|(before, _)| before.contains("suppressed for"))
        .map_or(message, |(_, repeated)| repeated);
    let text = match message.trim() {
        "" => format!("[{}] {what}", environment.to_uppercase()),
        message => format!("[{}] {what}: {message}", environment.to_uppercase()),
    };
    text.chars().take(250).collect()
}

pub async fn create(target: &Target, issue: &Value) -> Result<Ticket> {
    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?
        .post(format!("{}/rest/api/3/issue", target.url))
        .basic_auth(&target.email, Some(&target.token))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .body(issue.to_string())
        .send()
        .await
        .context("cannot reach Jira")?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!(
            "Jira answered HTTP {status}: {}",
            body.chars().take(300).collect::<String>()
        );
    }
    let created: Value =
        serde_json::from_str(&body).context("Jira answered something unreadable")?;
    let key = created["key"]
        .as_str()
        .context("Jira's answer has no issue key")?
        .to_string();
    Ok(Ticket {
        url: format!("{}/browse/{key}", target.url),
        key,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_issue_carries_the_failure_and_where_it_happened() {
        let target = Target {
            url: "https://acme.atlassian.net".into(),
            email: String::new(),
            token: String::new(),
            project: "SHOP".into(),
            kind: "Bug".into(),
        };
        let known = Known {
            label: "error".into(),
            exception_class: Some("TypeError".into()),
            location: Some("app_x/cartridge/a.js:9".into()),
            example: "TypeError: boom".into(),
            first_seen: "2026-09-22T10:00:00Z".into(),
            last_seen: "2026-09-23T10:00:00Z".into(),
            count: 42,
            first_deploy_sha: Some("9451cff".into()),
            pending: false,
            back: false,
            resolved_at: None,
            muted: false,
            spiked_on: None,
            sites: Default::default(),
            controllers: Default::default(),
            orders: Default::default(),
        };
        let issue = issue(&target, "abc", "prd", &known, None, None);

        assert_eq!(issue["fields"]["project"]["key"], "SHOP");
        assert_eq!(
            issue["fields"]["summary"],
            "[PRD] TypeError at app_x/cartridge/a.js:9"
        );
        assert_eq!(issue["fields"]["labels"][1], "prd");
        let blocks = issue["fields"]["description"]["content"]
            .as_array()
            .unwrap();
        assert_eq!(blocks.last().unwrap()["type"], "codeBlock");
    }
}
