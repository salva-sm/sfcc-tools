//! `sfcc-upload sandbox`: the state of dw.json's on-demand sandbox, and start, stop or restart it.
//!
//! Every run records what ODS said under the sandbox's label, for the TUI to read. The last
//! line printed says the outcome on its own, which is all the TUI shows of a run.

use crate::logging;
use anyhow::{Context, Result, bail};
use sfcc_core::config::Config;
use sfcc_core::ods::{self, Ods, Operation, Sandbox};
use sfcc_core::state::{self, now_seconds, sandbox::Status};

pub async fn run(config: &Config, operation: Option<Operation>) -> Result<()> {
    let label = label(config)?;
    let (api, mut sandbox) = match find(config, &label).await {
        Ok(found) => found,
        Err(error) => {
            record_unknown(&label, &error);
            return Err(error);
        }
    };

    if let Some(operation) = operation {
        if !operation.allowed_in(&sandbox.state) {
            record(&label, &sandbox);
            bail!(
                "{label} is {} - {operation} needs it {}",
                sandbox.state,
                operation.needs()
            );
        }
        let before = sandbox.state.clone();
        api.operate(&sandbox, operation).await?;
        logging::ok(format!("{operation} requested for {label}"));
        // ODS takes a moment to move: right after, the list can still show the old state.
        sandbox = api.find(&label).await.unwrap_or_else(|_| sandbox.clone());
        if sandbox.state == before {
            sandbox.state = moving_to(operation).to_string();
        }
    }

    record(&label, &sandbox);
    crate::out!("{}", describe(&label, &sandbox));
    Ok(())
}

/// One line for `status`: the state, or why there is none. Never an error.
pub async fn describe_for_status(config: &Config) -> String {
    let Ok(label) = label(config) else {
        return "not an on-demand sandbox".to_string();
    };
    match find(config, &label).await {
        Ok((_, sandbox)) => {
            record(&label, &sandbox);
            describe(&label, &sandbox)
        }
        Err(error) => {
            record_unknown(&label, &error);
            format!("unknown: {error:#}")
        }
    }
}

fn label(config: &Config) -> Result<String> {
    ods::label(&config.hostname).with_context(|| {
        format!(
            "{} is not an on-demand sandbox - the Sandbox API only knows those",
            config.hostname
        )
    })
}

async fn find(config: &Config, label: &str) -> Result<(Ods, Sandbox)> {
    let api = Ods::new(config)?;
    let sandbox = api.find(label).await?;
    Ok((api, sandbox))
}

/// What to show until ODS lists the operation's effect.
fn moving_to(operation: Operation) -> &'static str {
    match operation {
        Operation::Start | Operation::Restart => "starting",
        Operation::Stop => "stopping",
    }
}

fn describe(label: &str, sandbox: &Sandbox) -> String {
    match sandbox.eol.as_deref().and_then(|eol| eol.get(..10)) {
        Some(day) => format!("{label} {} - deleted by ODS on {day}", sandbox.state),
        None => format!("{label} {}", sandbox.state),
    }
}

fn record(label: &str, sandbox: &Sandbox) {
    let status = Status {
        label: label.to_string(),
        state: sandbox.state.clone(),
        eol: sandbox.eol.clone(),
        detail: None,
        at: now_seconds(),
    };
    state::write(&state::sandbox::path(label), &status);
}

fn record_unknown(label: &str, error: &anyhow::Error) {
    let status = Status {
        label: label.to_string(),
        state: "unknown".to_string(),
        eol: None,
        detail: Some(format!("{error:#}")),
        at: now_seconds(),
    };
    state::write(&state::sandbox::path(label), &status);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sandbox(state: &str, eol: Option<&str>) -> Sandbox {
        Sandbox {
            id: "2c3d".into(),
            realm: "zzzz".into(),
            instance: "001".into(),
            state: state.into(),
            host_name: "zzzz-001.dx.commercecloud.salesforce.com".into(),
            eol: eol.map(str::to_string),
        }
    }

    #[test]
    fn says_the_state_and_the_day_ods_deletes_it() {
        assert_eq!(
            describe("zzzz-001", &sandbox("started", None)),
            "zzzz-001 started"
        );
        assert_eq!(
            describe(
                "zzzz-001",
                &sandbox("stopped", Some("2026-12-01T00:00:00Z"))
            ),
            "zzzz-001 stopped - deleted by ODS on 2026-12-01"
        );
    }

    #[test]
    fn an_operation_shows_as_under_way_until_ods_lists_it() {
        assert_eq!(moving_to(Operation::Start), "starting");
        assert_eq!(moving_to(Operation::Restart), "starting");
        assert_eq!(moving_to(Operation::Stop), "stopping");
        assert!(ods::is_transition(moving_to(Operation::Stop)));
    }
}
