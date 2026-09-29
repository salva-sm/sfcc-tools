//! Reads the state the other tools write, and runs them for anything that acts.

use std::io::{BufRead, BufReader, Read};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use ratatui::Frame;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Paragraph, Row, Table, TableState};
use sfcc_core::daemon;
use sfcc_core::state::{self, errors, now_seconds, sessions, upload};

const REFRESH: Duration = Duration::from_secs(1);
const ACTIVITY_LINES: usize = 200;
const PROBE: Duration = Duration::from_millis(50);
const STREAM_LINES: usize = 2000;

struct Watcher {
    identity: String,
    status: upload::Status,
    heartbeat: Option<i64>,
}

impl Watcher {
    fn running(&self) -> bool {
        self.heartbeat
            .is_some_and(|age| age <= state::STALE_SECONDS)
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
    LogDiff,
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
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
    sandbox_log: Option<Stream>,
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
            .map(|(identity, status)| Watcher {
                heartbeat: upload::daemon(&identity).heartbeat_age(),
                identity,
                status,
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
    }

    fn current(&self) -> Option<&Watcher> {
        self.watchers.get(self.selected.selected()?)
    }

    fn move_by(&mut self, step: isize) {
        match self.focus {
            Focus::Panel => self.scroll = self.scroll.saturating_add_signed(-step),
            Focus::Watchers if !self.watchers.is_empty() => {
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
            Focus::Watchers => {}
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
        match view {
            View::SandboxLog if self.sandbox_log.is_none() => {
                let arguments = ["logger", "--color", "never", "--code-version", &version];
                self.sandbox_log = Some(Stream::start("sfcc-upload", &owned(&arguments), &dir));
            }
            _ => {}
        }
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
        let ran = Command::new(program)
            .args(&arguments)
            .current_dir(watcher.checkout())
            .stdin(Stdio::null())
            .output();
        self.notice = Some(match ran {
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
        });
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
        let command: Option<&[&str]> = match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return Ok(()),
            KeyCode::Tab => {
                app.focus = match app.focus {
                    Focus::Watchers => Focus::Panel,
                    Focus::Panel => Focus::Watchers,
                };
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                app.move_by(1);
                None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                app.move_by(-1);
                None
            }
            KeyCode::PageDown => {
                app.focus = Focus::Panel;
                app.move_by(page);
                None
            }
            KeyCode::PageUp => {
                app.focus = Focus::Panel;
                app.move_by(-page);
                None
            }
            KeyCode::End => {
                app.scroll = 0;
                None
            }
            KeyCode::Char('a') => {
                app.show(View::Uploads);
                None
            }
            KeyCode::Char('l') => {
                app.show(View::SandboxLog);
                None
            }
            KeyCode::Char('d') => {
                app.show(View::LogDiff);
                None
            }
            KeyCode::Char('p') => Some(&["sfcc-upload", "push"]),
            KeyCode::Char('s') => {
                app.quietly(&["sfcc-upload", "start"]);
                None
            }
            KeyCode::Char('x') => {
                app.quietly(&["sfcc-upload", "stop"]);
                None
            }
            KeyCode::Char('w') => {
                let running = app.current().is_some_and(|watcher| {
                    daemon::running(&errors::daemon(&watcher.identity)).is_some()
                });
                match running {
                    true => app.quietly(&["log-diff", "stop"]),
                    false => app.quietly(&["log-diff", "start"]),
                }
                None
            }
            KeyCode::Char('e') => Some(&["log-diff", "list", "--pending"]),
            _ => None,
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
        Constraint::Length(1),
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

    let running = |on: bool, name: &str| match on {
        true => format!("{name} ●"),
        false => name.to_string(),
    };
    let help = Line::from(vec![
        key("Tab", "focus".to_string()),
        key("↑↓", "move".to_string()),
        key("a", "uploads".to_string()),
        key("l", running(app.sandbox_log.is_some(), "sandbox log")),
        key("d", "log-diff".to_string()),
        key("w", "watch errors".to_string()),
        key("p", "push".to_string()),
        key("s", "start".to_string()),
        key("x", "stop".to_string()),
        key("e", "errors".to_string()),
        key("q", "quit".to_string()),
    ]);
    frame.render_widget(Paragraph::new(help), keys);
    if let Some(text) = &app.notice {
        frame.render_widget(
            Paragraph::new(text.clone()).style(Style::new().fg(Color::Yellow)),
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
            Constraint::Length(16),
            Constraint::Length(12),
            Constraint::Length(6),
            Constraint::Min(10),
        ],
    )
    .header(header(&[
        "STATE", "SANDBOX", "VERSION", "CHECKOUT", "BEAT", "",
    ]))
    .row_highlight_style(Style::new().reversed())
    .highlight_symbol("▶ ")
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

    #[test]
    fn shows_a_log_line_without_its_colours() {
        assert_eq!(strip_ansi("\u{1b}[32mOK\u{1b}[0m done"), "OK done");
    }
}
