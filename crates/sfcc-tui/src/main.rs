//! Reads the state the other tools write, and runs them for anything that acts.

mod actions;

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Read};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use actions::{Action, SHOWN};
use anyhow::Result;
use ratatui::Frame;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Paragraph, Row, Table, TableState};
use sfcc_core::daemon;
use sfcc_core::ods::{self, Operation};
use sfcc_core::state::{self, errors, now_seconds, sandbox, sessions, upload};

const REFRESH: Duration = Duration::from_secs(1);
const ACTIVITY_LINES: usize = 200;
const PROBE: Duration = Duration::from_millis(50);
const STREAM_LINES: usize = 2000;

struct Watcher {
    identity: String,
    status: upload::Status,
    heartbeat: Option<i64>,
    /// `zzzz-001`, when the watcher's host is an on-demand sandbox.
    label: Option<String>,
    /// What `sfcc-upload sandbox` last recorded about it.
    sandbox: Option<sandbox::Status>,
}

impl Watcher {
    fn running(&self) -> bool {
        self.heartbeat
            .is_some_and(|age| age <= state::STALE_SECONDS)
    }

    fn errors_watched(&self) -> bool {
        daemon::running(&errors::daemon(&self.identity)).is_some()
    }

    /// What `S` does to its sandbox, from the state ODS last reported.
    fn power(&self) -> Option<Operation> {
        let state = &self.sandbox.as_ref().filter(|it| it.is_known())?.state;
        [Operation::Start, Operation::Stop]
            .into_iter()
            .find(|operation| operation.allowed_in(state))
    }

    fn restartable(&self) -> bool {
        self.sandbox
            .as_ref()
            .is_some_and(|it| it.is_known() && Operation::Restart.allowed_in(&it.state))
    }

    /// Where `sfcc-upload` and `log-diff` find this checkout's dw.json.
    fn checkout(&self) -> PathBuf {
        let cartridges = Path::new(&self.status.cartridges);
        cartridges.parent().unwrap_or(cartridges).to_path_buf()
    }
}

/// A long-running command whose output the TUI shows; stopped with it.
struct Stream {
    child: Option<Child>,
    lines: Arc<Mutex<Vec<String>>>,
}

impl Stream {
    fn start(program: &str, arguments: &[String], dir: &Path) -> Stream {
        let lines = Arc::new(Mutex::new(vec![format!(
            "$ {program} {}",
            arguments.join(" ")
        )]));
        let spawned = Command::new(program)
            .args(arguments)
            .current_dir(dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let child = match spawned {
            Ok(mut child) => {
                if let Some(out) = child.stdout.take() {
                    follow(out, Arc::clone(&lines));
                }
                if let Some(err) = child.stderr.take() {
                    follow(err, Arc::clone(&lines));
                }
                Some(child)
            }
            Err(error) => {
                push_line(&lines, format!("cannot run {program}: {error}"));
                None
            }
        };
        Stream { child, lines }
    }

    fn lines(&self) -> Vec<String> {
        self.lines
            .lock()
            .map(|lines| lines.clone())
            .unwrap_or_default()
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
        }
    }
}

fn follow(source: impl Read + Send + 'static, lines: Arc<Mutex<Vec<String>>>) {
    std::thread::spawn(move || {
        for line in BufReader::new(source).lines().map_while(Result::ok) {
            push_line(&lines, strip_ansi(&line));
        }
    });
}

fn push_line(lines: &Mutex<Vec<String>>, line: String) {
    if let Ok(mut lines) = lines.lock() {
        lines.push(line);
        let excess = lines.len().saturating_sub(STREAM_LINES);
        lines.drain(..excess);
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum View {
    #[default]
    Uploads,
    SandboxLog,
    SandboxStatus,
    LogDiff,
}

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
enum Focus {
    #[default]
    Watchers,
    Panel,
}

#[derive(Default)]
struct App {
    watchers: Vec<Watcher>,
    /// With how its watcher runs: `start`ed, in a terminal or task, or not at all.
    errors: Vec<(errors::Status, Watching)>,
    /// What the last action said, until the next one.
    notice: Option<String>,
    /// With whether its adapter still answers: one killed outright leaves its file behind.
    sessions: Vec<(sessions::Session, bool)>,
    selected: TableState,
    activity: Vec<String>,
    view: View,
    focus: Focus,
    /// Lines up from the bottom; 0 follows the end.
    scroll: usize,
    /// Whether the panel holds more lines than it shows, as of the last frame.
    scrollable: bool,
    sandbox_log: Option<Stream>,
    /// Whether to ask ODS at all: not for `--print`, which draws once and leaves.
    live: bool,
    /// Sandbox labels with an `sfcc-upload sandbox` run under way.
    busy: Arc<Mutex<HashSet<String>>>,
    /// When each label was last asked for, in case a run records nothing.
    asked: HashMap<String, i64>,
    /// What background runs said, for the notice line.
    reports: Arc<Mutex<Vec<String>>>,
    /// An action shown as needing its key once more before it runs.
    armed: Option<Action>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Watching {
    Detached,
    Elsewhere,
    Stopped,
}

fn watching(identity: &str) -> Watching {
    let files = errors::daemon(identity);
    match (daemon::running(&files), files.is_beating()) {
        (Some(_), _) => Watching::Detached,
        (None, true) => Watching::Elsewhere,
        (None, false) => Watching::Stopped,
    }
}

impl App {
    fn refresh(&mut self) {
        self.watchers = upload::named()
            .into_iter()
            .map(|(identity, status)| {
                let label = ods::label(&status.hostname);
                Watcher {
                    heartbeat: upload::daemon(&identity).heartbeat_age(),
                    sandbox: label.as_deref().and_then(sandbox::of),
                    label,
                    identity,
                    status,
                }
            })
            .collect();
        self.watchers.sort_by_key(|watcher| !watcher.running());
        self.errors = errors::named()
            .into_iter()
            .filter(|(_, status)| now_seconds() - status.at <= errors::STALE_SECONDS)
            .map(|(identity, status)| (status, watching(&identity)))
            .collect();
        self.sessions = sessions::all()
            .into_iter()
            .map(|session| {
                let address = SocketAddr::from((Ipv4Addr::LOCALHOST, session.port));
                let alive = TcpStream::connect_timeout(&address, PROBE).is_ok();
                (session, alive)
            })
            .collect();
        let last = self.watchers.len().saturating_sub(1);
        match self.selected.selected() {
            Some(at) if at <= last => {}
            _ if self.watchers.is_empty() => self.selected.select(None),
            _ => self.selected.select(Some(0)),
        }
        self.activity = self
            .current()
            .map(|watcher| plain(upload::daemon(&watcher.identity).tail(ACTIVITY_LINES)))
            .unwrap_or_default();
        if self.live {
            self.ask_sandboxes();
        }
        if let Some(said) = self.reports.lock().ok().and_then(|mut it| it.pop()) {
            self.notice = Some(said);
        }
    }

    /// Runs `sfcc-upload sandbox status` for each sandbox whose recorded state is old enough,
    /// in the background: it goes to Account Manager and to ODS, and the screen must not wait.
    fn ask_sandboxes(&mut self) {
        let now = now_seconds();
        let mut seen = HashSet::new();
        let mut due = Vec::new();
        for watcher in &self.watchers {
            let Some(label) = watcher.label.clone() else {
                continue;
            };
            if !seen.insert(label.clone()) {
                continue;
            }
            let stale = watcher.sandbox.as_ref().is_none_or(sandbox::Status::is_due);
            // A run that records nothing (an sfcc-upload without `sandbox`) is not retried soon.
            let wait = match watcher.sandbox {
                Some(_) => sandbox::MOVING_SECONDS,
                None => sandbox::UNKNOWN_SECONDS,
            };
            let recent = self.asked.get(&label).is_some_and(|at| now - at < wait);
            if stale && !recent {
                due.push((label, watcher.checkout()));
            }
        }
        for (label, dir) in due {
            self.asked.insert(label.clone(), now);
            self.in_background(&label, &["sandbox", "status"], dir, false);
        }
    }

    /// `sfcc-upload <arguments>` for a sandbox, one run per label at a time.
    fn in_background(&self, label: &str, arguments: &[&str], dir: PathBuf, report: bool) {
        let Ok(mut busy) = self.busy.lock() else {
            return;
        };
        if !busy.insert(label.to_string()) {
            return;
        }
        let mut arguments = owned(arguments);
        arguments.extend(owned(&["--color", "never"]));
        let (busy, reports, label) = (
            Arc::clone(&self.busy),
            Arc::clone(&self.reports),
            label.to_string(),
        );
        std::thread::spawn(move || {
            let said = last_line("sfcc-upload", &arguments, &dir);
            if let Ok(mut busy) = busy.lock() {
                busy.remove(&label);
            }
            if report && let Ok(mut reports) = reports.lock() {
                reports.push(said);
            }
        });
    }

    fn is_busy(&self, label: &str) -> bool {
        self.busy.lock().is_ok_and(|busy| busy.contains(label))
    }

    /// What the action does for the selected row, or `None` when it does nothing there.
    fn label(&self, action: Action) -> Option<String> {
        let watcher = self.current();
        let selected = watcher.is_some();
        if home(action).is_some_and(|view| view != self.view) {
            return None;
        }
        // The sandbox's keys wait for a run in progress.
        let sandbox_idle = watcher
            .and_then(|it| it.label.as_deref())
            .is_some_and(|label| !self.is_busy(label));
        let text = |on: bool, what: &str| on.then(|| what.to_string());
        match action {
            // Choosing a panel leaves the focus in it, so with several watchers it must stay
            // possible to go back and choose another one.
            Action::Focus => text(
                self.scrollable || (self.focus == Focus::Panel && self.watchers.len() > 1),
                "focus",
            ),
            Action::Move(_) => match self.arrows_choose() {
                true => Some("move".to_string()),
                false => text(self.scrollable, "scroll"),
            },
            Action::Page(_) => text(self.scrollable, "page"),
            Action::Follow => text(self.scroll > 0, "follow"),
            Action::Uploads => text(selected && self.view != View::Uploads, "uploads"),
            Action::SandboxLog => match self.sandbox_log.is_some() {
                true => text(selected && self.view != View::SandboxLog, "sandbox log ●"),
                false => text(selected, "sandbox log"),
            },
            Action::SandboxStatus => text(
                watcher.is_some_and(|it| it.label.is_some()) && self.view != View::SandboxStatus,
                "sandbox status",
            ),
            Action::LogDiff => text(selected && self.view != View::LogDiff, "log-diff"),
            Action::WatchErrors => watcher.map(|it| match it.errors_watched() {
                true => "stop watching errors".to_string(),
                false => "watch errors".to_string(),
            }),
            Action::Push => text(selected, "push"),
            Action::StartWatcher => watcher
                .filter(|it| !it.running())
                .map(|_| "start".to_string()),
            Action::StopWatcher => watcher
                .filter(|it| it.running())
                .map(|_| "stop".to_string()),
            Action::Errors => text(selected, "errors"),
            Action::PowerSandbox => watcher
                .filter(|_| sandbox_idle)
                .and_then(Watcher::power)
                .map(|operation| format!("{operation} sandbox")),
            Action::RestartSandbox => text(
                sandbox_idle && watcher.is_some_and(Watcher::restartable),
                "restart sandbox",
            ),
            Action::Quit => text(true, "quit"),
        }
    }

    /// Taking the sandbox down asks for its key twice: everyone on it loses it.
    fn needs_confirmation(&self, action: Action) -> bool {
        match action {
            Action::RestartSandbox => true,
            Action::PowerSandbox => self
                .current()
                .and_then(Watcher::power)
                .is_some_and(|operation| operation == Operation::Stop),
            _ => false,
        }
    }

    /// Whether the action may run now; the first press of one that needs confirming arms it.
    fn confirmed(&mut self, action: Action, what: &str) -> bool {
        if !self.needs_confirmation(action) || self.armed == Some(action) {
            self.armed = None;
            return true;
        }
        self.armed = Some(action);
        self.notice = Some(format!(
            "{} again to {what} - any other key cancels",
            action.keys()
        ));
        false
    }

    /// Runs the action, or returns the command that takes the terminal over.
    fn perform(&mut self, action: Action, page: isize) -> Option<&'static [&'static str]> {
        let Some(what) = self.label(action) else {
            self.armed = None;
            return None;
        };
        if !self.confirmed(action, &what) {
            return None;
        }
        match action {
            Action::Focus => {
                self.focus = match self.focus {
                    Focus::Watchers => Focus::Panel,
                    Focus::Panel => Focus::Watchers,
                };
            }
            Action::Move(step) => self.move_by(step),
            Action::Page(direction) => {
                self.focus = Focus::Panel;
                self.move_by(direction * page);
            }
            Action::Follow => self.scroll = 0,
            Action::Uploads => self.show(View::Uploads),
            Action::SandboxLog => self.show(View::SandboxLog),
            Action::SandboxStatus => self.show(View::SandboxStatus),
            Action::LogDiff => self.show(View::LogDiff),
            Action::WatchErrors => match self.current().is_some_and(Watcher::errors_watched) {
                true => self.quietly(&["log-diff", "stop"]),
                false => self.quietly(&["log-diff", "start"]),
            },
            Action::Push => return Some(&["sfcc-upload", "push"]),
            Action::StartWatcher => self.quietly(&["sfcc-upload", "start"]),
            Action::StopWatcher => self.quietly(&["sfcc-upload", "stop"]),
            Action::Errors => return Some(&["log-diff", "list", "--pending"]),
            Action::PowerSandbox => {
                if let Some(operation) = self.current().and_then(Watcher::power) {
                    self.operate(operation);
                }
            }
            Action::RestartSandbox => self.operate(Operation::Restart),
            Action::Quit => {}
        }
        None
    }

    fn operate(&mut self, operation: Operation) {
        let Some(watcher) = self.current() else {
            return;
        };
        let Some(label) = watcher.label.clone() else {
            return;
        };
        let dir = watcher.checkout();
        self.notice = Some(format!("{operation} requested for {label}…"));
        self.in_background(&label, &["sandbox", operation.name()], dir, true);
    }

    fn current(&self) -> Option<&Watcher> {
        self.watchers.get(self.selected.selected()?)
    }

    /// Whether the arrows choose a watcher: with one there is nothing to choose, and they scroll.
    fn arrows_choose(&self) -> bool {
        self.focus == Focus::Watchers && self.watchers.len() > 1
    }

    fn move_by(&mut self, step: isize) {
        match self.focus {
            Focus::Watchers if self.arrows_choose() => {
                let at = self.selected.selected().unwrap_or(0) as isize + step;
                let at = at.clamp(0, self.watchers.len() as isize - 1) as usize;
                if Some(at) != self.selected.selected() {
                    self.selected.select(Some(at));
                    // Their output is another checkout's.
                    self.sandbox_log = None;
                    self.view = View::Uploads;
                    self.scroll = 0;
                }
            }
            Focus::Panel | Focus::Watchers => {
                self.scroll = self.scroll.saturating_add_signed(-step);
            }
        }
    }

    fn show(&mut self, view: View) {
        self.view = view;
        self.scroll = 0;
        self.focus = Focus::Panel;
        let Some(watcher) = self.current() else {
            return;
        };
        let dir = watcher.checkout();
        let version = watcher.status.code_version.clone();
        let label = watcher.label.clone();
        match (view, label) {
            (View::SandboxLog, _) if self.sandbox_log.is_none() => {
                let arguments = ["logger", "--color", "never", "--code-version", &version];
                self.sandbox_log = Some(Stream::start("sfcc-upload", &owned(&arguments), &dir));
            }
            // Opening it asks ODS again, rather than showing what a minute ago said.
            (View::SandboxStatus, Some(label)) => {
                self.in_background(&label, &["sandbox", "status"], dir, false);
            }
            _ => {}
        }
    }

    /// The sandbox as ODS last reported it, one fact to a line.
    fn sandbox_lines(&self) -> Vec<String> {
        let Some(watcher) = self.current() else {
            return Vec::new();
        };
        let Some(label) = &watcher.label else {
            return vec![format!(
                "{} is not an on-demand sandbox - the Sandbox API only knows those",
                watcher.status.hostname
            )];
        };
        let mut lines = vec![
            format!("sandbox      {label}"),
            format!("host         {}", watcher.status.hostname),
        ];
        match &watcher.sandbox {
            None => lines.push("state        not asked yet".to_string()),
            Some(status) => {
                let moving = match self.is_busy(label) {
                    true => " (asking ODS…)",
                    false => "",
                };
                lines.push(format!("state        {}{moving}", status.state));
                lines.push(format!("checked      {}", ago(status.at)));
                if let Some(day) = status.eol.as_deref().and_then(|eol| eol.get(..10)) {
                    lines.push(format!("deleted by   ODS on {day}"));
                }
                if let Some(detail) = &status.detail {
                    lines.push(String::new());
                    lines.push(format!("ODS said: {detail}"));
                }
            }
        }
        lines
    }

    fn panel(&self) -> (String, Vec<String>) {
        let version = self
            .current()
            .map(|watcher| watcher.status.code_version.clone())
            .unwrap_or_default();
        match self.view {
            View::Uploads => (format!(" Uploads · {version} "), self.activity.clone()),
            View::SandboxLog => (
                format!(" Sandbox log · {version} "),
                self.sandbox_log
                    .as_ref()
                    .map(Stream::lines)
                    .unwrap_or_default(),
            ),
            View::SandboxStatus => (
                format!(
                    " Sandbox · {} ",
                    self.current()
                        .and_then(|watcher| watcher.label.clone())
                        .unwrap_or_default()
                ),
                self.sandbox_lines(),
            ),
            View::LogDiff => (
                format!(" log-diff · {version} "),
                self.current()
                    .map(|watcher| plain(errors::daemon(&watcher.identity).tail(ACTIVITY_LINES)))
                    .unwrap_or_default(),
            ),
        }
    }
}

impl App {
    /// For what ends at once: its last line is all there is to say.
    fn quietly(&mut self, command: &[&str]) {
        let Some(watcher) = self.current() else {
            return;
        };
        let [program, arguments @ ..] = command else {
            return;
        };
        let mut arguments = owned(arguments);
        if *program == "sfcc-upload" {
            arguments.extend([
                "--code-version".to_string(),
                watcher.status.code_version.clone(),
            ]);
        }
        self.notice = Some(last_line(program, &arguments, &watcher.checkout()));
    }
}

/// The panel an action belongs to: it is listed, and answers, only while that one is shown.
/// Moving, focus and choosing a panel belong to none.
fn home(action: Action) -> Option<View> {
    match action {
        Action::Push | Action::StartWatcher | Action::StopWatcher => Some(View::Uploads),
        Action::WatchErrors | Action::Errors => Some(View::LogDiff),
        Action::PowerSandbox | Action::RestartSandbox => Some(View::SandboxStatus),
        _ => None,
    }
}

/// Runs a command to its end; its last line is all there is to say.
fn last_line(program: &str, arguments: &[String], dir: &Path) -> String {
    let ran = Command::new(program)
        .args(arguments)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output();
    match ran {
        Ok(output) => {
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            text.lines()
                .map(strip_ansi)
                .map(|line| line.trim().to_string())
                .rfind(|line| !line.is_empty())
                .unwrap_or_else(|| format!("{program} {}: done", arguments.join(" ")))
        }
        Err(error) => format!("cannot run {program}: {error}"),
    }
}

fn owned(arguments: &[&str]) -> Vec<String> {
    arguments
        .iter()
        .map(|argument| argument.to_string())
        .collect()
}

const USAGE: &str = concat!(
    "sfcc-tui ",
    env!("CARGO_PKG_VERSION"),
    "

",
    "One terminal screen for the SFCC tools: the uploader's watchers, the errors
",
    "log-diff has pending, and debug sessions. It reads the state they write and
",
    "runs them for anything that acts.

",
    "  --print     draw one frame to stdout and exit
",
    "  --version   print the version

",
    "https://github.com/salva-sm/sfcc-tools"
);

fn main() -> Result<()> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let asked = |names: &[&str]| arguments.iter().any(|it| names.contains(&it.as_str()));
    if asked(&["--version", "-V"]) {
        println!("sfcc-tui {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if asked(&["--help", "-h"]) {
        println!("{USAGE}");
        return Ok(());
    }

    let mut app = App::default();
    app.refresh();

    if asked(&["--print"]) {
        let mut terminal = ratatui::Terminal::new(TestBackend::new(118, 34))?;
        terminal.draw(|frame| draw(frame, &mut app))?;
        let buffer = terminal.backend().buffer();
        for y in 0..buffer.area.height {
            let row: String = (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect();
            println!("{}", row.trim_end());
        }
        return Ok(());
    }

    app.live = true;
    app.refresh();
    let mut terminal = ratatui::init();
    let outcome = run(&mut terminal, &mut app);
    ratatui::restore();
    outcome
}

fn run(terminal: &mut ratatui::DefaultTerminal, app: &mut App) -> Result<()> {
    loop {
        terminal.draw(|frame| draw(frame, app))?;
        if !event::poll(REFRESH)? {
            app.refresh();
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        let page = terminal.size()?.height as isize / 2;
        let command = match Action::of(key.code, key.modifiers) {
            Some(Action::Quit) => return Ok(()),
            Some(action) => app.perform(action, page),
            None => {
                app.armed = None;
                None
            }
        };
        let checkout = app
            .current()
            .map(|watcher| (watcher.checkout(), watcher.status.code_version.clone()));
        if let (Some([program, arguments @ ..]), Some((dir, version))) = (command, checkout) {
            let mut arguments = owned(arguments);
            if *program == "sfcc-upload" {
                arguments.extend(["--code-version".to_string(), version]);
            }
            ratatui::restore();
            hand_over(program, &arguments, &dir);
            *terminal = ratatui::init();
        }
        app.refresh();
    }
}

/// The terminal is the command's until it ends, so `push` can ask before overwriting.
fn hand_over(program: &str, arguments: &[String], dir: &Path) {
    println!("$ {program} {}\n", arguments.join(" "));
    if let Err(error) = Command::new(program)
        .args(arguments)
        .current_dir(dir)
        .status()
    {
        println!("cannot run {program}: {error}");
    }
    println!("\nEnter to go back");
    let _ = std::io::stdin().read_line(&mut String::new());
}

fn draw(frame: &mut Frame, app: &mut App) {
    let [watchers, middle, panel, notice, keys] = Layout::vertical([
        Constraint::Length(app.watchers.len().max(1) as u16 + 3),
        Constraint::Length(app.errors.len().max(app.sessions.len()).max(1) as u16 + 3),
        Constraint::Min(5),
        Constraint::Length(1),
        // The keys wrap rather than fall off a narrow terminal.
        Constraint::Length(2),
    ])
    .areas(frame.area());
    let [errors_area, sessions_area] =
        Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).areas(middle);

    let border = |focus: Focus| match app.focus == focus {
        true => Style::new().fg(Color::Cyan),
        false => Style::new(),
    };
    let (watcher_border, panel_border) = (border(Focus::Watchers), border(Focus::Panel));
    frame.render_stateful_widget(
        watcher_table(app).block(
            Block::bordered()
                .title(" Watchers ")
                .border_style(watcher_border),
        ),
        watchers,
        &mut app.selected,
    );
    frame.render_widget(error_table(app), errors_area);
    frame.render_widget(session_table(app), sessions_area);

    let (title, lines) = app.panel();
    let height = panel.height.saturating_sub(2) as usize;
    app.scrollable = lines.len() > height;
    app.scroll = app.scroll.min(lines.len().saturating_sub(height));
    let end = lines.len() - app.scroll;
    let shown: Vec<Line> = lines[end.saturating_sub(height)..end]
        .iter()
        .map(|line| activity_line(line))
        .collect();
    let title = match app.scroll {
        0 => title,
        up => format!("{title}· {up} lines up, End to follow "),
    };
    frame.render_widget(
        Paragraph::new(shown).block(Block::bordered().title(title).border_style(panel_border)),
        panel,
    );

    let help: Vec<Span> = SHOWN
        .iter()
        .filter_map(|action| Some(key(action.keys(), app.label(*action)?)))
        .collect();
    frame.render_widget(Paragraph::new(wrapped(help, keys.width)), keys);

    // Why the sandbox's state is unknown, while no action has anything to say.
    let unknown = app
        .current()
        .and_then(|watcher| watcher.sandbox.as_ref()?.detail.clone())
        .map(|detail| format!("ODS: {detail}"));
    if let Some(text) = app.notice.clone().or(unknown) {
        frame.render_widget(
            Paragraph::new(text).style(Style::new().fg(Color::Yellow)),
            notice,
        );
    }
}

fn watcher_table(app: &App) -> Table<'static> {
    let rows: Vec<Row> = app
        .watchers
        .iter()
        .map(|watcher| {
            let (state, colour) = match (watcher.running(), watcher.status.state) {
                (false, _) => ("stopped", Color::DarkGray),
                (true, upload::State::Synced) => ("synced", Color::Green),
                (true, upload::State::Uploading) => ("uploading", Color::Yellow),
                (true, upload::State::Failed) => ("failed", Color::Red),
                (true, upload::State::Stopped) => ("stopped", Color::DarkGray),
            };
            let detail = match (&watcher.status.detail, watcher.status.files) {
                (Some(detail), _) => detail.clone(),
                (None, 0) => String::new(),
                (None, files) => format!("{files} file(s)"),
            };
            Row::new(vec![
                Cell::from(state).style(Style::new().fg(colour).add_modifier(Modifier::BOLD)),
                Cell::from(short_host(&watcher.status.hostname)),
                sandbox_cell(app, watcher),
                Cell::from(watcher.status.code_version.clone()),
                Cell::from(checkout_name(&watcher.status.cartridges)),
                Cell::from(match watcher.heartbeat {
                    Some(age) => format!("{age}s"),
                    None => "-".to_string(),
                }),
                Cell::from(detail),
            ])
        })
        .collect();
    Table::new(
        rows,
        [
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Length(16),
            Constraint::Length(12),
            Constraint::Length(6),
            Constraint::Min(10),
        ],
    )
    .header(header(&[
        "STATE", "SANDBOX", "ODS", "VERSION", "CHECKOUT", "BEAT", "",
    ]))
    .row_highlight_style(Style::new().reversed())
    .highlight_symbol("▶ ")
}

/// The sandbox's state as ODS last reported it: red when it cannot take an upload.
fn sandbox_cell(app: &App, watcher: &Watcher) -> Cell<'static> {
    let Some(label) = &watcher.label else {
        return Cell::from("");
    };
    let Some(status) = &watcher.sandbox else {
        return Cell::from("?").style(Style::new().fg(Color::DarkGray));
    };
    let colour = match status.state.as_str() {
        _ if !status.is_known() => Color::DarkGray,
        "started" => Color::Green,
        "stopped" | "failed" | "deleting" | "deleted" => Color::Red,
        state if ods::is_transition(state) => Color::Yellow,
        _ => Color::Gray,
    };
    let text = match app.is_busy(label) {
        true => format!("{}…", status.state),
        false => status.state.clone(),
    };
    Cell::from(text).style(Style::new().fg(colour))
}

fn error_table(app: &App) -> Table<'static> {
    let rows: Vec<Row> = app
        .errors
        .iter()
        .map(|(status, watching)| {
            let pending = Cell::from(status.pending.to_string()).style(match status.pending {
                0 => Style::new().fg(Color::Green),
                _ => Style::new().fg(Color::Red).bold(),
            });
            let (watch, colour) = match watching {
                Watching::Detached => ("watching", Color::Green),
                Watching::Elsewhere => ("in a task", Color::Green),
                Watching::Stopped => ("stopped", Color::DarkGray),
            };
            Row::new(vec![
                Cell::from(watch).style(Style::new().fg(colour).add_modifier(Modifier::BOLD)),
                Cell::from(short_host(&status.hostname)),
                Cell::from(checkout_name(&status.cartridges)),
                pending,
                Cell::from(status.new.to_string()),
                Cell::from(ago(status.at)),
            ])
        })
        .collect();
    Table::new(
        rows,
        [
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Length(8),
            Constraint::Length(4),
            Constraint::Min(6),
        ],
    )
    .header(header(&[
        "LOG-DIFF", "SANDBOX", "CHECKOUT", "PENDING", "NEW", "CHECKED",
    ]))
    .block(Block::bordered().title(" SFCC errors · log-diff "))
}

fn session_table(app: &App) -> Table<'static> {
    let rows: Vec<Row> = app
        .sessions
        .iter()
        .map(|(session, alive)| {
            let (state, colour) = match alive {
                true => ("attached", Color::Green),
                false => ("gone", Color::DarkGray),
            };
            Row::new(vec![
                Cell::from(state).style(Style::new().fg(colour).add_modifier(Modifier::BOLD)),
                Cell::from(checkout_name(&session.cartridges)),
                Cell::from(format!("127.0.0.1:{}", session.port)),
            ])
        })
        .collect();
    let title = match app.sessions.iter().any(|(_, alive)| *alive) {
        true => " Debugging ",
        false => " Debugging · none attached ",
    };
    Table::new(
        rows,
        [
            Constraint::Length(9),
            Constraint::Length(12),
            Constraint::Min(10),
        ],
    )
    .header(header(&["STATE", "CHECKOUT", "HOVER ENDPOINT"]))
    .block(Block::bordered().title(title))
}

fn header(names: &[&'static str]) -> Row<'static> {
    Row::new(names.to_vec()).style(Style::new().fg(Color::Cyan).bold())
}

fn key(name: &'static str, what: String) -> Span<'static> {
    Span::from(format!(" {name} {what} ")).style(Style::new().fg(Color::Black).bg(Color::Gray))
}

/// The keys on as many lines as the width needs, never splitting one across two.
fn wrapped(keys: Vec<Span<'static>>, width: u16) -> Vec<Line<'static>> {
    let mut lines = vec![Line::default()];
    for key in keys {
        let used = lines.last().map_or(0, Line::width);
        if used > 0 && used + key.width() > usize::from(width) {
            lines.push(Line::default());
        }
        if let Some(line) = lines.last_mut() {
            line.push_span(key);
        }
    }
    lines
}

fn activity_line(line: &str) -> Line<'static> {
    let colour = if line.contains(" x ") || line.contains("error") {
        Color::Red
    } else if line.contains(" ! ") {
        Color::Yellow
    } else if line.contains(" OK ") || line.contains(" -> ") {
        Color::Green
    } else {
        Color::Gray
    };
    Line::from(line.to_string()).style(Style::new().fg(colour))
}

/// `abcd-003.my.commercecloud...` as `abcd-003`.
fn short_host(host: &str) -> String {
    host.split('.').next().unwrap_or(host).to_string()
}

/// `C:\...\sfcc-eu\source\cartridges` as `sfcc-eu`.
fn checkout_name(cartridges: &str) -> String {
    Path::new(cartridges)
        .ancestors()
        .skip(1)
        .filter_map(|folder| folder.file_name()?.to_str())
        .find(|name| *name != "source")
        .unwrap_or(cartridges)
        .to_string()
}

fn ago(at: i64) -> String {
    match now_seconds() - at {
        seconds if seconds < 60 => format!("{seconds}s ago"),
        seconds if seconds < 3600 => format!("{}m ago", seconds / 60),
        seconds => format!("{}h ago", seconds / 3600),
    }
}

fn plain(lines: Vec<String>) -> Vec<String> {
    lines.iter().map(|line| strip_ansi(line)).collect()
}

fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for next in chars.by_ref() {
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_a_checkout_by_its_folder_not_by_source() {
        assert_eq!(checkout_name("/work/sfcc-eu/source/cartridges"), "sfcc-eu");
        assert_eq!(checkout_name("/work/shop/cartridges"), "shop");
    }

    #[test]
    fn a_stream_collects_what_its_command_prints_or_why_it_could_not_run() {
        let stream = Stream::start("rustc", &owned(&["--version"]), Path::new("."));
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !stream.lines().iter().any(|line| line.starts_with("rustc ")) {
            assert!(std::time::Instant::now() < deadline, "{:?}", stream.lines());
            std::thread::sleep(Duration::from_millis(20));
        }
        let missing = Stream::start("no-such-program-here", &[], Path::new("."));
        assert!(missing.lines()[1].starts_with("cannot run no-such-program-here"));
    }

    fn watcher_on(state: Option<&str>, detail: Option<&str>) -> Watcher {
        Watcher {
            identity: "zzzz-005_dx__develop".into(),
            status: upload::Status {
                state: upload::State::Synced,
                cartridges: "/work/sfcc-eu/source/cartridges".into(),
                hostname: "zzzz-005.dx.commercecloud.salesforce.com".into(),
                code_version: "develop".into(),
                files: 0,
                detail: None,
                at: 0,
            },
            heartbeat: Some(1),
            label: Some("zzzz-005".into()),
            sandbox: state.map(|state| sandbox::Status {
                label: "zzzz-005".into(),
                state: state.into(),
                eol: None,
                detail: detail.map(str::to_string),
                at: now_seconds(),
            }),
        }
    }

    #[test]
    fn the_sandbox_keys_follow_the_state_ods_reported() {
        let started = watcher_on(Some("started"), None);
        assert_eq!(started.power(), Some(Operation::Stop));
        assert!(started.restartable());

        let stopped = watcher_on(Some("stopped"), None);
        assert_eq!(stopped.power(), Some(Operation::Start));
        assert!(!stopped.restartable());

        let starting = watcher_on(Some("starting"), None);
        assert_eq!(starting.power(), None);
        assert!(!starting.restartable());

        assert_eq!(watcher_on(None, None).power(), None);
        let unknown = watcher_on(Some("unknown"), Some("no Sandbox API access"));
        assert_eq!(unknown.power(), None);
    }

    #[test]
    fn scrolling_is_listed_only_when_the_panel_has_more_than_it_shows() {
        let mut app = App {
            watchers: vec![watcher_on(Some("started"), None)],
            focus: Focus::Panel,
            ..App::default()
        };
        app.selected.select(Some(0));
        assert_eq!(app.label(Action::Move(1)), None);
        assert_eq!(app.label(Action::Page(1)), None);
        assert_eq!(app.label(Action::Follow), None);
        assert_eq!(app.label(Action::Focus), None);

        app.scrollable = true;
        assert_eq!(app.label(Action::Move(1)).as_deref(), Some("scroll"));
        assert_eq!(app.label(Action::Page(1)).as_deref(), Some("page"));
    }

    #[test]
    fn with_one_watcher_the_arrows_scroll_the_panel_and_tab_still_works() {
        let mut app = App {
            watchers: vec![watcher_on(Some("started"), None)],
            scrollable: true,
            ..App::default()
        };
        app.selected.select(Some(0));
        assert_eq!(app.focus, Focus::Watchers);
        assert_eq!(app.label(Action::Move(-1)).as_deref(), Some("scroll"));
        assert_eq!(app.label(Action::Focus).as_deref(), Some("focus"));
        app.move_by(-3);
        assert_eq!(app.scroll, 3);
        assert_eq!(app.selected.selected(), Some(0));
        app.perform(Action::Focus, 10);
        assert_eq!(app.focus, Focus::Panel);
    }

    #[test]
    fn with_several_watchers_the_way_back_to_them_stays_listed() {
        let mut app = App {
            watchers: vec![
                watcher_on(Some("started"), None),
                watcher_on(Some("stopped"), None),
            ],
            focus: Focus::Panel,
            ..App::default()
        };
        app.selected.select(Some(0));
        assert_eq!(app.label(Action::Focus).as_deref(), Some("focus"));
        app.focus = Focus::Watchers;
        assert_eq!(app.label(Action::Focus), None);
        assert_eq!(app.label(Action::Move(1)).as_deref(), Some("move"));
    }

    #[test]
    fn each_action_is_listed_only_in_the_panel_it_belongs_to() {
        let mut app = App {
            watchers: vec![watcher_on(Some("started"), None)],
            ..App::default()
        };
        app.selected.select(Some(0));
        let listed = |app: &App| -> Vec<Action> {
            [
                Action::Push,
                Action::StopWatcher,
                Action::WatchErrors,
                Action::Errors,
            ]
            .into_iter()
            .filter(|action| app.label(*action).is_some())
            .collect()
        };
        app.view = View::Uploads;
        assert_eq!(listed(&app), vec![Action::Push, Action::StopWatcher]);
        app.view = View::LogDiff;
        assert_eq!(listed(&app), vec![Action::WatchErrors, Action::Errors]);
        app.view = View::SandboxLog;
        assert!(listed(&app).is_empty());
    }

    #[test]
    fn the_sandbox_keys_show_only_in_the_sandbox_panel() {
        let mut app = App {
            watchers: vec![watcher_on(Some("started"), None)],
            ..App::default()
        };
        app.selected.select(Some(0));
        for view in [View::Uploads, View::SandboxLog, View::LogDiff] {
            app.view = view;
            assert_eq!(app.label(Action::PowerSandbox), None);
            assert_eq!(app.label(Action::RestartSandbox), None);
        }
        app.view = View::SandboxStatus;
        assert!(app.label(Action::PowerSandbox).is_some());
        assert!(app.label(Action::RestartSandbox).is_some());
        assert_eq!(app.label(Action::SandboxStatus), None);
    }

    #[test]
    fn the_status_panel_says_what_ods_reported_and_why_it_could_not() {
        let mut app = App {
            watchers: vec![watcher_on(Some("stopped"), None)],
            view: View::SandboxStatus,
            ..App::default()
        };
        app.selected.select(Some(0));
        let (title, lines) = app.panel();
        assert_eq!(title, " Sandbox · zzzz-005 ");
        assert!(lines.contains(&"state        stopped".to_string()));

        app.watchers = vec![watcher_on(Some("unknown"), Some("no Sandbox API access"))];
        let lines = app.panel().1;
        assert!(lines.contains(&"ODS said: no Sandbox API access".to_string()));

        let mut elsewhere = watcher_on(None, None);
        elsewhere.label = None;
        app.watchers = vec![elsewhere];
        assert_eq!(app.label(Action::SandboxStatus), None);
        assert!(app.panel().1[0].contains("not an on-demand sandbox"));
    }

    #[test]
    fn only_taking_the_sandbox_down_asks_twice() {
        let mut app = App {
            watchers: vec![watcher_on(Some("started"), None)],
            view: View::SandboxStatus,
            ..App::default()
        };
        app.selected.select(Some(0));
        assert_eq!(
            app.label(Action::PowerSandbox).as_deref(),
            Some("stop sandbox")
        );
        assert!(!app.confirmed(Action::PowerSandbox, "stop sandbox"));
        assert_eq!(app.armed, Some(Action::PowerSandbox));
        assert!(app.confirmed(Action::PowerSandbox, "stop sandbox"));
        assert_eq!(app.armed, None);

        app.watchers = vec![watcher_on(Some("stopped"), None)];
        assert_eq!(
            app.label(Action::PowerSandbox).as_deref(),
            Some("start sandbox")
        );
        assert!(app.confirmed(Action::PowerSandbox, "start sandbox"));
        assert_eq!(app.label(Action::RestartSandbox), None);
    }

    #[test]
    fn a_key_that_does_not_fit_goes_whole_to_the_next_line() {
        let keys = vec![
            key("q", "quit".to_string()),
            key("S", "stop sandbox".to_string()),
        ];
        let lines = wrapped(keys, 20);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1].to_string(), " S stop sandbox ");
        assert_eq!(wrapped(Vec::new(), 20).len(), 1);
    }

    #[test]
    fn shows_a_log_line_without_its_colours() {
        assert_eq!(strip_ansi("\u{1b}[32mOK\u{1b}[0m done"), "OK done");
    }
}
