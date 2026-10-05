//! Every key the TUI answers to. The help line and the dispatch read this one table, and
//! `App::label` says what each does for the selected row, or that it does nothing there: a
//! key is shown exactly when it acts.

use ratatui::crossterm::event::{KeyCode, KeyModifiers};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    Focus,
    /// One row, or one line of the panel.
    Move(isize),
    /// Half a screen of the panel.
    Page(isize),
    Follow,
    Uploads,
    SandboxLog,
    /// What the Sandbox API says about the sandbox, and the keys that act on it.
    SandboxStatus,
    LogDiff,
    WatchErrors,
    Push,
    StartWatcher,
    StopWatcher,
    Errors,
    /// Start a stopped sandbox, or stop a started one.
    PowerSandbox,
    RestartSandbox,
    Quit,
}

/// In the help line's order. Paging and the second arrow act without a place of their own.
pub const SHOWN: [Action; 15] = [
    Action::Focus,
    Action::Move(1),
    Action::Follow,
    Action::Uploads,
    Action::SandboxLog,
    Action::SandboxStatus,
    Action::LogDiff,
    Action::WatchErrors,
    Action::Push,
    Action::StartWatcher,
    Action::StopWatcher,
    Action::Errors,
    Action::PowerSandbox,
    Action::RestartSandbox,
    Action::Quit,
];

impl Action {
    pub fn of(code: KeyCode, modifiers: KeyModifiers) -> Option<Action> {
        Some(match code {
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => Action::Quit,
            KeyCode::Char('q') | KeyCode::Esc => Action::Quit,
            KeyCode::Tab => Action::Focus,
            KeyCode::Down | KeyCode::Char('j') => Action::Move(1),
            KeyCode::Up | KeyCode::Char('k') => Action::Move(-1),
            KeyCode::PageDown => Action::Page(1),
            KeyCode::PageUp => Action::Page(-1),
            KeyCode::End => Action::Follow,
            KeyCode::Char('a') => Action::Uploads,
            KeyCode::Char('l') => Action::SandboxLog,
            KeyCode::Char('o') => Action::SandboxStatus,
            KeyCode::Char('d') => Action::LogDiff,
            KeyCode::Char('w') => Action::WatchErrors,
            KeyCode::Char('p') => Action::Push,
            KeyCode::Char('s') => Action::StartWatcher,
            KeyCode::Char('x') => Action::StopWatcher,
            KeyCode::Char('e') => Action::Errors,
            // Upper case: it spends realm credits, or takes the sandbox down for everyone on it.
            KeyCode::Char('S') => Action::PowerSandbox,
            KeyCode::Char('R') => Action::RestartSandbox,
            _ => return None,
        })
    }

    /// As the help line writes the key.
    pub fn keys(self) -> &'static str {
        match self {
            Action::Focus => "Tab",
            Action::Move(_) => "↑↓",
            Action::Page(_) => "PgUp PgDn",
            Action::Follow => "End",
            Action::Uploads => "a",
            Action::SandboxLog => "l",
            Action::SandboxStatus => "o",
            Action::LogDiff => "d",
            Action::WatchErrors => "w",
            Action::Push => "p",
            Action::StartWatcher => "s",
            Action::StopWatcher => "x",
            Action::Errors => "e",
            Action::PowerSandbox => "S",
            Action::RestartSandbox => "R",
            Action::Quit => "q",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shown_action_has_a_key_that_reaches_it() {
        let keys = [
            KeyCode::Tab,
            KeyCode::Down,
            KeyCode::End,
            KeyCode::Char('a'),
            KeyCode::Char('l'),
            KeyCode::Char('o'),
            KeyCode::Char('d'),
            KeyCode::Char('w'),
            KeyCode::Char('p'),
            KeyCode::Char('s'),
            KeyCode::Char('x'),
            KeyCode::Char('e'),
            KeyCode::Char('S'),
            KeyCode::Char('R'),
            KeyCode::Char('q'),
        ];
        let reached: Vec<Action> = keys
            .iter()
            .filter_map(|code| Action::of(*code, KeyModifiers::NONE))
            .collect();
        assert_eq!(reached, SHOWN.to_vec());
    }

    #[test]
    fn the_sandbox_keys_are_upper_case_only() {
        assert_eq!(
            Action::of(KeyCode::Char('S'), KeyModifiers::SHIFT),
            Some(Action::PowerSandbox)
        );
        assert_eq!(
            Action::of(KeyCode::Char('s'), KeyModifiers::NONE),
            Some(Action::StartWatcher)
        );
        assert_eq!(Action::of(KeyCode::Char('r'), KeyModifiers::NONE), None);
    }

    #[test]
    fn control_c_quits_and_a_plain_c_does_nothing() {
        assert_eq!(
            Action::of(KeyCode::Char('c'), KeyModifiers::CONTROL),
            Some(Action::Quit)
        );
        assert_eq!(Action::of(KeyCode::Char('c'), KeyModifiers::NONE), None);
    }
}
