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
    /// The previous or next panel, so the arrows walk the views as ↑↓ walk rows.
    View(isize),
    Follow,
    Uploads,
    SandboxLog,
    /// What the Sandbox API says about the sandbox, and the keys that act on it.
    SandboxStatus,
    LogDiff,
    /// Files the watcher left as the sandbox has them.
    Conflicts,
    /// Send this checkout's version of the selected held file, over whatever the sandbox has.
    Overwrite,
    /// Stop holding the selected file, keeping the sandbox copy.
    Keep,
    WatchErrors,
    Push,
    StartWatcher,
    StopWatcher,
    Errors,
    /// Switch the sandbox log between the raw record and the cleaned one.
    RawLog,
    /// Show the hovered sandbox-log line as its whole raw record.
    Reveal,
    /// Start a stopped sandbox, or stop a started one.
    PowerSandbox,
    RestartSandbox,
    Quit,
}

/// Grouped as the help line shows them: the panels first, then what acts on the
/// selected watcher. Paging and the second arrow of each pair have no place of
/// their own.
pub const SHOWN: [Action; 21] = [
    Action::Uploads,
    Action::SandboxLog,
    Action::LogDiff,
    Action::SandboxStatus,
    Action::Conflicts,
    Action::View(1),
    Action::Focus,
    Action::Move(1),
    Action::Follow,
    Action::Overwrite,
    Action::Keep,
    Action::WatchErrors,
    Action::Push,
    Action::StartWatcher,
    Action::StopWatcher,
    Action::Errors,
    Action::RawLog,
    Action::Reveal,
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
            KeyCode::Right => Action::View(1),
            KeyCode::Left => Action::View(-1),
            KeyCode::End => Action::Follow,
            KeyCode::Char('u') => Action::Uploads,
            KeyCode::Char('l') => Action::SandboxLog,
            KeyCode::Char('t') => Action::SandboxStatus,
            KeyCode::Char('d') => Action::LogDiff,
            KeyCode::Char('c') => Action::Conflicts,
            KeyCode::Char('O') => Action::Overwrite,
            KeyCode::Char('K') => Action::Keep,
            KeyCode::Char('w') => Action::WatchErrors,
            KeyCode::Char('p') => Action::Push,
            KeyCode::Char('s') => Action::StartWatcher,
            KeyCode::Char('x') => Action::StopWatcher,
            KeyCode::Char('e') => Action::Errors,
            KeyCode::Char('r') => Action::RawLog,
            KeyCode::Enter => Action::Reveal,
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
            Action::View(_) => "←→",
            Action::Follow => "End",
            Action::Uploads => "u",
            Action::SandboxLog => "l",
            Action::SandboxStatus => "t",
            Action::LogDiff => "d",
            Action::Conflicts => "c",
            Action::Overwrite => "O",
            Action::Keep => "K",
            Action::WatchErrors => "w",
            Action::Push => "p",
            Action::StartWatcher => "s",
            Action::StopWatcher => "x",
            Action::Errors => "e",
            Action::RawLog => "r",
            Action::Reveal => "⏎",
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
            KeyCode::Char('u'),
            KeyCode::Char('l'),
            KeyCode::Char('d'),
            KeyCode::Char('t'),
            KeyCode::Char('c'),
            KeyCode::Right,
            KeyCode::Tab,
            KeyCode::Down,
            KeyCode::End,
            KeyCode::Char('O'),
            KeyCode::Char('K'),
            KeyCode::Char('w'),
            KeyCode::Char('p'),
            KeyCode::Char('s'),
            KeyCode::Char('x'),
            KeyCode::Char('e'),
            KeyCode::Char('r'),
            KeyCode::Enter,
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
        // Lower-case r is the log toggle, not the restart; restart stays upper-case.
        assert_eq!(
            Action::of(KeyCode::Char('r'), KeyModifiers::NONE),
            Some(Action::RawLog)
        );
        assert_eq!(
            Action::of(KeyCode::Char('R'), KeyModifiers::SHIFT),
            Some(Action::RestartSandbox)
        );
    }

    #[test]
    fn control_c_quits_and_a_plain_c_opens_conflicts() {
        assert_eq!(
            Action::of(KeyCode::Char('c'), KeyModifiers::CONTROL),
            Some(Action::Quit)
        );
        assert_eq!(
            Action::of(KeyCode::Char('c'), KeyModifiers::NONE),
            Some(Action::Conflicts)
        );
    }
}
