//! The on-demand sandbox a dw.json points at, as the Sandbox API (ODS) knows it.
//!
//! Only that one sandbox: never the realm's list, never create, clone or delete. What may be
//! done to it depends on its state, and that rule lives here so the TUI can show only the
//! keys that apply without linking an HTTP client.

use crate::config::{Instance, classify_host};
use std::fmt;

#[cfg(feature = "ods")]
#[path = "ods_client.rs"]
mod client;
#[cfg(feature = "ods")]
pub use client::{Ods, Sandbox};

/// The state ODS reports while a sandbox moves between two others.
const TRANSITIONS: [&str; 6] = [
    "new",
    "creating",
    "starting",
    "stopping",
    "resetting",
    "upgrading",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    Start,
    Stop,
    Restart,
}

impl Operation {
    /// The name ODS takes in `POST /sandboxes/{id}/operations`.
    pub fn name(self) -> &'static str {
        match self {
            Operation::Start => "start",
            Operation::Stop => "stop",
            Operation::Restart => "restart",
        }
    }

    /// Starting a running sandbox, or stopping a stopped one, is refused by ODS anyway.
    pub fn allowed_in(self, state: &str) -> bool {
        match self {
            Operation::Start => state == "stopped",
            Operation::Stop | Operation::Restart => state == "started",
        }
    }

    /// The state it needs, for saying why it was not done.
    pub fn needs(self) -> &'static str {
        match self {
            Operation::Start => "stopped",
            Operation::Stop | Operation::Restart => "started",
        }
    }
}

impl fmt::Display for Operation {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// `zzzz-001` for `zzzz-001.dx.commercecloud.salesforce.com`, and nothing for a host that is
/// not an on-demand sandbox. The instance answers on `.dx.` and `.my.`, and ODS lists only
/// `.dx.`, so a sandbox is matched on this first label, never on the whole hostname.
pub fn label(hostname: &str) -> Option<String> {
    if classify_host(hostname) != Instance::Sandbox {
        return None;
    }
    let label = hostname.split('.').next()?.to_lowercase();
    let (realm, instance) = label.split_once(['-', '_'])?;
    let is_name = |part: &str| {
        !part.is_empty()
            && part
                .chars()
                .all(|character| character.is_ascii_alphanumeric())
    };
    (realm.len() == 4 && is_name(realm) && is_name(instance)).then(|| format!("{realm}-{instance}"))
}

/// The realm a sandbox label belongs to: `zzzz` for `zzzz-001`.
pub fn realm(label: &str) -> &str {
    label.split(['-', '_']).next().unwrap_or(label)
}

/// A state that will change on its own, so it is worth asking again soon.
pub fn is_transition(state: &str) -> bool {
    TRANSITIONS.contains(&state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sandbox_is_named_by_its_first_label_on_either_domain() {
        assert_eq!(
            label("zzzz-001.dx.commercecloud.salesforce.com").as_deref(),
            Some("zzzz-001")
        );
        assert_eq!(
            label("ZZZZ-001.my.commercecloud.salesforce.com").as_deref(),
            Some("zzzz-001")
        );
        assert_eq!(
            label("zzpq_013.dx.commercecloud.salesforce.com").as_deref(),
            Some("zzpq-013")
        );
    }

    #[test]
    fn a_host_that_is_not_an_on_demand_sandbox_has_no_label() {
        assert_eq!(label("development-eu01-shop.demandware.net"), None);
        assert_eq!(label("localhost"), None);
        assert_eq!(label("toolong-001.dx.commercecloud.salesforce.com"), None);
        assert_eq!(label("zzzz.dx.commercecloud.salesforce.com"), None);
    }

    #[test]
    fn the_realm_is_the_part_before_the_instance() {
        assert_eq!(realm("zzzz-001"), "zzzz");
    }

    #[test]
    fn an_operation_is_allowed_only_from_the_state_it_leaves() {
        assert!(Operation::Start.allowed_in("stopped"));
        assert!(!Operation::Start.allowed_in("started"));
        assert!(!Operation::Start.allowed_in("stopping"));
        assert!(Operation::Stop.allowed_in("started"));
        assert!(Operation::Restart.allowed_in("started"));
        assert!(!Operation::Restart.allowed_in("stopped"));
        assert!(!Operation::Stop.allowed_in("unknown"));
    }

    #[test]
    fn a_moving_state_is_a_transition_and_a_settled_one_is_not() {
        assert!(is_transition("starting"));
        assert!(is_transition("stopping"));
        assert!(!is_transition("started"));
        assert!(!is_transition("stopped"));
        assert!(!is_transition("failed"));
    }
}
