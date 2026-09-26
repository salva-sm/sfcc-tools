use crate::envs::Environment;
use crate::ledger::{Known, serious};
use crate::notify::{envelope, headline};
use crate::output::message;
use crate::team::Team;
use chrono::{Duration, Utc};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const TOP: usize = 5;

pub struct Week {
    pub name: String,
    pub total: u64,
    /// Records in the window before, for the trend.
    pub before: u64,
    /// Share of the records that show as an error page.
    pub serious_share: f64,
    pub new: Vec<Line>,
    pub top: Vec<Line>,
    pub growing: Vec<Line>,
    pub spikes: usize,
}

pub struct Line {
    pub headline: String,
    pub count: u64,
    pub before: u64,
    pub serious: bool,
    pub ticket: Option<String>,
}

/// Leaves out what is muted.
pub fn weeks(environments: &[Environment], team: &Team, days: i64) -> Vec<Week> {
    let today = Utc::now().date_naive();
    let day = |back: i64| {
        (today - Duration::days(back))
            .format("%Y-%m-%d")
            .to_string()
    };
    let window: Vec<String> = (0..days).map(day).collect();
    let previous: Vec<String> = (days..days * 2).map(day).collect();
    let since = day(days - 1);

    environments
        .iter()
        .map(|environment| {
            let ledger = &environment.ledger;
            let sum = |days: &[String]| -> BTreeMap<&str, u64> {
                let mut counts = BTreeMap::new();
                for day in days {
                    for (id, count) in ledger.daily.get(day).into_iter().flatten() {
                        *counts.entry(id.as_str()).or_default() += count;
                    }
                }
                counts
            };
            let now = sum(&window);
            let then = sum(&previous);
            let line = |id: &str, known: &Known| Line {
                headline: named(known),
                count: now.get(id).copied().unwrap_or(0),
                before: then.get(id).copied().unwrap_or(0),
                serious: serious(&known.label, &known.example),
                ticket: team.tickets.get(id).map(|ticket| ticket.key.clone()),
            };
            let visible = || {
                ledger
                    .known_signatures
                    .iter()
                    .filter(|(id, _)| !team.mutes(id))
            };

            let mut new: Vec<Line> = visible()
                .filter(|(_, known)| known.first_seen.get(..10).unwrap_or("") >= since.as_str())
                .map(|(id, known)| line(id, known))
                .collect();
            new.sort_by(|left, right| {
                right
                    .serious
                    .cmp(&left.serious)
                    .then(right.count.cmp(&left.count))
            });

            let mut top: Vec<Line> = visible()
                .map(|(id, known)| line(id, known))
                .filter(|line| line.count > 0)
                .collect();
            top.sort_by(|left, right| {
                right
                    .serious
                    .cmp(&left.serious)
                    .then(right.count.cmp(&left.count))
            });

            let mut growing: Vec<Line> = visible()
                .map(|(id, known)| line(id, known))
                .filter(|line| line.before > 0 && line.count >= line.before * 2 && line.count >= 20)
                .collect();
            growing.sort_by(|left, right| {
                let ratio = |line: &Line| line.count as f64 / line.before as f64;
                ratio(right).total_cmp(&ratio(left))
            });

            let total: u64 = now.values().sum();
            let serious_total: u64 = now
                .iter()
                .filter(|(id, _)| {
                    ledger
                        .known_signatures
                        .get(**id)
                        .is_some_and(|known| serious(&known.label, &known.example))
                })
                .map(|(_, count)| count)
                .sum();

            Week {
                name: environment.name.clone(),
                total,
                before: then.values().sum(),
                serious_share: match total {
                    0 => 0.0,
                    total => serious_total as f64 / total as f64,
                },
                spikes: ledger
                    .known_signatures
                    .values()
                    .filter(|known| {
                        known
                            .spiked_on
                            .as_deref()
                            .is_some_and(|on| on >= since.as_str())
                    })
                    .count(),
                new: new.into_iter().take(TOP).collect(),
                top: top.into_iter().take(TOP).collect(),
                growing: growing.into_iter().take(TOP).collect(),
            }
        })
        .collect()
}

/// Without an exception, the message: `customerror: Basket <n> has no shipment` says more
/// than the level.
fn named(known: &Known) -> String {
    match known.exception_class {
        Some(_) => headline(
            &known.label,
            known.exception_class.as_deref(),
            known.location.as_deref(),
        ),
        None => {
            let head = known.example.lines().next().unwrap_or_default();
            let text: String = message(head, None).chars().take(80).collect();
            match &known.location {
                Some(location) => format!("{}: {text} ({location})", known.label),
                None => format!("{}: {text}", known.label),
            }
        }
    }
}

pub fn trend(now: u64, before: u64) -> String {
    match before {
        0 if now == 0 => "=".to_string(),
        0 => "new".to_string(),
        before => {
            let change = (now as f64 - before as f64) / before as f64 * 100.0;
            format!("{}{:.0}%", if change >= 0.0 { "+" } else { "" }, change)
        }
    }
}

pub fn card(weeks: &[Week], days: i64, dashboard: Option<&str>) -> Value {
    let mut body = vec![json!({
        "type": "TextBlock",
        "size": "Medium",
        "weight": "Bolder",
        "wrap": true,
        "text": format!("SFCC errors, last {days} days"),
    })];
    for week in weeks {
        body.push(json!({
            "type": "TextBlock",
            "weight": "Bolder",
            "wrap": true,
            "separator": true,
            "spacing": "Large",
            "text": week.name.to_uppercase(),
        }));
        body.push(json!({ "type": "FactSet", "facts": [
            { "title": "Records", "value": format!("{} ({} vs the {days} days before)", week.total, trend(week.total, week.before)) },
            { "title": "Error pages", "value": format!("{:.0}% of them", week.serious_share * 100.0) },
            { "title": "New signatures", "value": week.new.len().to_string() },
            { "title": "Spikes", "value": week.spikes.to_string() },
        ]}));
        for (title, lines) in [
            ("New", &week.new),
            ("Most logged", &week.top),
            ("Growing", &week.growing),
        ] {
            if lines.is_empty() {
                continue;
            }
            let text = lines
                .iter()
                .map(|line| {
                    let mark = if line.serious { "**500** " } else { "" };
                    let ticket = line
                        .ticket
                        .as_deref()
                        .map(|key| format!(" · {key}"))
                        .unwrap_or_default();
                    format!(
                        "- {mark}{} · x{} ({}){ticket}",
                        line.headline,
                        line.count,
                        trend(line.count, line.before)
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            body.push(json!({ "type": "TextBlock", "weight": "Bolder", "text": title, "spacing": "Medium" }));
            body.push(json!({ "type": "TextBlock", "wrap": true, "text": text }));
        }
    }
    let actions = dashboard
        .map(|url| vec![json!({ "type": "Action.OpenUrl", "title": "Dashboard", "url": url })])
        .unwrap_or_default();
    envelope(body, actions)
}

#[cfg(test)]
mod tests {
    use super::trend;

    #[test]
    fn a_trend_is_a_percentage_or_says_it_is_new() {
        assert_eq!(trend(150, 100), "+50%");
        assert_eq!(trend(90, 100), "-10%");
        assert_eq!(trend(5, 0), "new");
        assert_eq!(trend(0, 0), "=");
    }
}
