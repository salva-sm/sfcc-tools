mod ci;
mod finding;
mod ledger;
mod local;
mod normalize;
mod notify;
mod output;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand, ValueHint};
use local::{Local, reachable};
use output::{Badge, Card, Tone, status};
use sfcc_core::config::Config;
use sfcc_core::logs::parse_levels;
use sfcc_core::webdav::Dav;
use std::path::PathBuf;
use std::time::Duration;

/// Errors only. Warnings and custom info logs change too often to be news.
const DEFAULT_LEVELS: &str = "error,customerror,fatal";

/// Exit status when there is something new, as opposed to a failure.
const EXIT_NEW: i32 = 1;
/// Exit status when log-diff itself could not do its job.
const EXIT_FAILED: i32 = 2;
/// How long `check` waits for the sandbox before giving up on it.
const PROBE_LIMIT: Duration = Duration::from_secs(15);

const EXAMPLES: &str = "\
Examples:
  log-diff check                       one pass over the sandbox log, now
  log-diff check --fail-on-new         the same, exiting 1 while anything is pending (a git hook)
  log-diff watch                       the same pass every 10s, until Ctrl-C
  log-diff ack                         list what is pending; `ack <id>` or `ack --all` to clear it
  log-diff run --config dev/dw.json    read DEV, keeping the team's ledger on this machine
  log-diff run --state ledger.json --sha 9451cff --build 4821
                                       CI: record a deploy and update the team's ledger
  log-diff notify --report new.json    CI: post what `run --report` found to Teams

The team's ledger comes from --shared or LOG_DIFF_SHARED: a path to a clone of the
ledger repository, or a raw URL (with LOG_DIFF_TOKEN or GITHUB_TOKEN when it is private).
With neither, the one `run` keeps on this machine is used, if there is one; with none at
all, check and watch still work, comparing against what you have seen.";

#[derive(Parser)]
#[command(
    name = "log-diff",
    version,
    about = "Tell the SFCC errors a change introduced from the ones already known",
    after_help = EXAMPLES
)]
struct Cli {
    /// Colour the output: auto, always, never
    #[arg(
        long,
        global = true,
        value_name = "WHEN",
        default_value = "auto",
        value_parser = ["auto", "always", "never"]
    )]
    color: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// One pass: read the sandbox log since the last one, report what is new
    #[command(
        long_about = "One pass over the sandbox log: read what was logged since the \
        last pass - today's log, the first time - and report every signature neither the team \
        nor you had seen.\n\nNeeds nothing running beforehand. When `watch` is running, what \
        it already reported is not reported again.\n\nExits 0 normally, 1 with --fail-on-new \
        while something is pending, 2 when it could not check at all. An unreachable sandbox \
        is not a failure: there is nothing to check."
    )]
    Check(CheckArgs),
    /// The same pass on a timer, for a terminal or an editor task
    Watch(WatchArgs),
    /// CI: update the team's ledger from the shared instance's log
    #[command(
        long_about = "Update the team's ledger from the log of the instance in dw.json.\n\n\
        With --sha, a deploy is recorded first. Each new signature is laid at the deploy that \
        was live when it was first logged, with the commits since the deploy before as the \
        suspects. The first run has nothing to compare against, and learns instead of \
        reporting: today's log, or --baseline-days more. Only what the instance still keeps \
        in its log folder is read; log_archive is not."
    )]
    Run(RunArgs),
    /// CI: post a report written by `run --report` to a Teams channel
    Notify(NotifyArgs),
    /// List pending signatures, or mark them as dealt with
    Ack(AckArgs),
    /// Print the shell completion script: bash, zsh, fish, powershell or elvish
    #[command(
        long_about = "Print the shell completion script for SHELL on stdout.\n\n\
        Git Bash, bash with bash-completion and fish need nothing: every run of log-diff \
        keeps the script where they look. For zsh, add `eval \"$(log-diff completions zsh)\"` \
        to ~/.zshrc; for PowerShell, this to the $PROFILE:\n\n\
        \x20 log-diff completions powershell | Out-String | Invoke-Expression"
    )]
    Completions(CompletionsArgs),
}

#[derive(Args)]
struct CompletionsArgs {
    /// The shell to complete for
    #[arg(value_name = "SHELL")]
    shell: clap_complete::Shell,
}

#[derive(Args)]
struct InstanceArgs {
    /// Path to dw.json (default: the nearest one, searching upwards)
    #[arg(long, short = 'c', value_name = "PATH", value_hint = ValueHint::FilePath)]
    config: Option<PathBuf>,
    /// Log levels to read, comma separated, or "all"
    #[arg(long, value_name = "LIST", default_value = DEFAULT_LEVELS)]
    level: String,
}

#[derive(Args)]
struct LocalArgs {
    #[command(flatten)]
    instance: InstanceArgs,
    /// Your own ledger (default: log-diff/local-ledger.json in the user config directory)
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    state: Option<PathBuf>,
    /// The team's ledger: a path, or a URL (default: the one `run` keeps on this machine,
    /// when there is one)
    #[arg(long, value_name = "PATH|URL", env = "LOG_DIFF_SHARED")]
    shared: Option<String>,
    /// No desktop notification, only the terminal
    #[arg(long)]
    no_desktop: bool,
    /// Print `path:line: error:` lines for an editor's problem matcher instead of cards
    #[arg(long)]
    problems: bool,
    /// Resolve a pending signature not logged again for this long: 3d, 36h, or 0 for never
    #[arg(
        long,
        value_name = "DURATION",
        default_value = "3d",
        env = "LOG_DIFF_EXPIRE",
        value_parser = parse_span
    )]
    expire: Duration,
}

#[derive(Args)]
struct CheckArgs {
    #[command(flatten)]
    local: LocalArgs,
    /// Exit 1 while any signature is pending, new in this pass or not
    #[arg(long)]
    fail_on_new: bool,
    /// Take everything logged since the last pass as known, and report nothing
    #[arg(long)]
    baseline: bool,
}

#[derive(Args)]
struct WatchArgs {
    #[command(flatten)]
    local: LocalArgs,
    /// Time between passes: 10s, 2m, or plain seconds
    #[arg(long, value_name = "DURATION", default_value = "10s", value_parser = parse_interval)]
    interval: Duration,
}

#[derive(Args)]
struct RunArgs {
    #[command(flatten)]
    instance: InstanceArgs,
    /// The team's ledger: ledger.json in a checkout of its repository on CI (default:
    /// log-diff/dev-ledger.json in the user config directory, which check and watch read)
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    state: Option<PathBuf>,
    /// The commit just deployed
    #[arg(long, value_name = "SHA")]
    sha: Option<String>,
    /// The CI build that deployed it
    #[arg(long, value_name = "N", requires = "sha")]
    build: Option<u64>,
    /// When the deploy went live, RFC 3339 (default: now)
    #[arg(long, value_name = "TIMESTAMP", requires = "sha")]
    at: Option<String>,
    /// Write what is new here, for `notify`
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    report: Option<PathBuf>,
    /// Link to the commits of a deploy, with {from} and {to} for the two shas
    #[arg(long, value_name = "URL", env = "LOG_DIFF_COMPARE_URL")]
    compare_url: Option<String>,
    /// Exit 1 when something new was found
    #[arg(long)]
    fail_on_new: bool,
    /// On the first run, learn from this many days of log before today, not only today's
    #[arg(long, value_name = "DAYS", default_value_t = 0)]
    baseline_days: u32,
}

#[derive(Args)]
struct NotifyArgs {
    /// Teams webhook (a Workflows webhook, or a legacy incoming webhook). Without one,
    /// nothing is sent and nothing fails
    #[arg(
        long,
        value_name = "URL",
        env = "LOG_DIFF_WEBHOOK",
        hide_env_values = true
    )]
    webhook: Option<String>,
    /// The report `run --report` wrote
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    report: PathBuf,
}

#[derive(Args)]
struct AckArgs {
    /// Signature ids, or the start of them
    #[arg(value_name = "ID")]
    ids: Vec<String>,
    /// Every pending signature
    #[arg(long, conflicts_with = "ids")]
    all: bool,
    /// It does not matter: never report it again, even if it keeps being logged
    #[arg(long)]
    mute: bool,
    /// Your own ledger
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    state: Option<PathBuf>,
}

#[tokio::main]
async fn main() {
    // Before parsing, which exits on --help and --version: any run counts.
    sfcc_core::completions::install::<Cli>("log-diff", env!("CARGO_PKG_VERSION"));
    match run(Cli::parse()).await {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            output::error(&format!("{error:#}"));
            std::process::exit(EXIT_FAILED);
        }
    }
}

async fn run(cli: Cli) -> Result<i32> {
    let problems = match &cli.command {
        Command::Check(args) => args.local.problems,
        Command::Watch(args) => args.local.problems,
        _ => false,
    };
    output::configure(&cli.color, problems);
    match cli.command {
        Command::Check(args) => check(args).await,
        Command::Watch(args) => {
            let (local, dav) = local(args.local)?;
            local.watch(&dav, args.interval).await?;
            Ok(0)
        }
        Command::Run(args) => ci_run(args).await,
        Command::Notify(args) => {
            let report = notify::Report::load(&args.report)?;
            if report.new.is_empty() {
                status(Tone::Ok, "nothing new to notify");
                return Ok(0);
            }
            let Some(webhook) = args.webhook.filter(|webhook| !webhook.trim().is_empty()) else {
                status(
                    Tone::Info,
                    &format!(
                        "{} new signature(s), but no Teams webhook (LOG_DIFF_WEBHOOK) - nothing sent",
                        report.new.len()
                    ),
                );
                return Ok(0);
            };
            notify::teams(&webhook, &report).await?;
            status(
                Tone::Ok,
                &format!("posted {} new signature(s) to Teams", report.new.len()),
            );
            Ok(0)
        }
        Command::Completions(args) => {
            sfcc_core::completions::print::<Cli>(args.shell, "log-diff");
            Ok(0)
        }
        Command::Ack(args) => {
            let state = args.state.unwrap_or_else(default_state);
            local::acknowledge(&state, &args.ids, args.all, args.mute)?;
            Ok(0)
        }
    }
}

async fn check(args: CheckArgs) -> Result<i32> {
    let (local, dav) = local(args.local)?;
    // A git hook waits on this, and a host that drops packets would otherwise
    // take every retry's timeout before anyone could commit.
    let probe = tokio::time::timeout(PROBE_LIMIT, reachable(&dav)).await;
    let unreachable = match probe {
        Ok(result) => result?,
        Err(_) => Some(format!("no answer in {}s", PROBE_LIMIT.as_secs())),
    };
    if let Some(reason) = unreachable {
        status(
            Tone::Warn,
            &format!(
                "{} unreachable ({reason}) - nothing checked",
                local.config.hostname
            ),
        );
        return Ok(0);
    }

    let team = local.team().await?;
    let outcome = local.pass(&dav, &team, args.baseline).await?;
    if args.baseline {
        status(
            Tone::Ok,
            "baseline taken; what the sandbox logged so far is known now",
        );
        return Ok(0);
    }
    local.report(&outcome);

    if args.fail_on_new && !outcome.pending.is_empty() {
        status(
            Tone::Info,
            "or commit with --no-verify to skip the check once",
        );
        return Ok(EXIT_NEW);
    }
    Ok(0)
}

async fn ci_run(args: RunArgs) -> Result<i32> {
    let config = Config::load(args.instance.config.clone(), None)?;
    let dav = Dav::new(&config)?;
    if let Some(reason) = reachable(&dav).await? {
        bail!("{} unreachable: {reason}", config.hostname);
    }

    let options = ci::RunOptions {
        levels: parse_levels(&args.instance.level),
        state: args.state.unwrap_or_else(team_on_this_machine),
        sha: args.sha,
        build: args.build,
        at: args.at,
        report: args.report,
        compare_url: args.compare_url,
        baseline_days: args.baseline_days,
    };
    let had_cursor = ledger::Ledger::load(&options.state)?
        .cursor_for(&config.hostname)
        .is_some();
    if had_cursor && args.baseline_days > 0 {
        status(
            Tone::Warn,
            &format!(
                "--baseline-days ignored: {} already has a baseline - delete it to start over",
                options.state.display()
            ),
        );
    }
    let outcome = ci::run(&config, &dav, &options).await?;

    if outcome.baseline {
        status(
            Tone::Ok,
            &format!(
                "first run on {}: {} signature(s) learned from the log since {}, nothing reported",
                config.hostname, outcome.known, outcome.baseline_from
            ),
        );
        return Ok(0);
    }
    let new = outcome.report.new.len();
    match new {
        0 => status(
            Tone::Ok,
            &format!("{} · nothing new, {} known", config.hostname, outcome.known),
        ),
        _ => status(
            Tone::New,
            &format!(
                "{new} new error{} on {} · {} known",
                if new == 1 { "" } else { "s" },
                config.hostname,
                outcome.known
            ),
        ),
    }
    for item in &outcome.report.new {
        let deploy = item.deploy.as_ref().map(|deploy| match deploy.build {
            Some(build) => format!("deploy {} (build {build})", short(&deploy.sha)),
            None => format!("deploy {}", short(&deploy.sha)),
        });
        let card = Card {
            id: &item.id,
            label: &item.label,
            exception: item.exception_class.as_deref(),
            location: item.location.as_deref(),
            example: &item.example,
            count: item.count,
            first_seen: &item.first_seen,
            last_seen: None,
            badge: Badge::New,
            deploy,
        };
        println!(
            "
{}",
            output::card(&card)
        );
    }

    match args.fail_on_new && !outcome.report.new.is_empty() {
        true => Ok(EXIT_NEW),
        false => Ok(0),
    }
}

fn local(args: LocalArgs) -> Result<(Local, Dav)> {
    let config = Config::load(args.instance.config.clone(), None)?;
    let dav = Dav::new(&config)?;
    // CI has no desktop, and a pop-up from a hook run by a GUI client is noise.
    let desktop = !args.no_desktop && std::env::var_os("CI").is_none();
    let local = Local {
        config,
        levels: parse_levels(&args.instance.level),
        state: args.state.unwrap_or_else(default_state),
        shared: args
            .shared
            .filter(|shared| !shared.trim().is_empty())
            .or_else(|| {
                let here = team_on_this_machine();
                here.is_file().then(|| here.to_string_lossy().into_owned())
            }),
        desktop,
        expire: Some(args.expire).filter(|expire| !expire.is_zero()),
    };
    Ok((local, dav))
}

fn short(sha: &str) -> &str {
    sha.get(..9).unwrap_or(sha)
}

fn default_state() -> PathBuf {
    ledger::local_dir().join("local-ledger.json")
}

/// With no ledger repository yet, `run` against DEV from this machine keeps
/// the team's ledger here, and `check` and `watch` pick it up on their own.
fn team_on_this_machine() -> PathBuf {
    ledger::local_dir().join("dev-ledger.json")
}

/// Time between passes: never less than a second.
fn parse_interval(raw: &str) -> Result<Duration> {
    Ok(parse_span(raw)?.max(Duration::from_secs(1)))
}

/// `10s`, `2m`, `36h`, `3d`, or plain seconds.
fn parse_span(raw: &str) -> Result<Duration> {
    let raw = raw.trim();
    let (number, unit) = match raw.find(|c: char| !c.is_ascii_digit()) {
        Some(at) => raw.split_at(at),
        None => (raw, "s"),
    };
    let value: u64 = number
        .parse()
        .with_context(|| format!("{raw:?} is not a duration like 10s, 2m, 36h or 3d"))?;
    let seconds = match unit {
        "s" => value,
        "m" => value * 60,
        "h" => value * 3_600,
        "d" => value * 86_400,
        _ => bail!("{raw:?} is not a duration like 10s, 2m, 36h or 3d"),
    };
    Ok(Duration::from_secs(seconds))
}

#[cfg(test)]
mod tests {
    use super::{parse_interval, parse_span};
    use std::time::Duration;

    #[test]
    fn reads_a_duration_in_seconds_minutes_hours_or_days() {
        assert_eq!(parse_span("10s").unwrap(), Duration::from_secs(10));
        assert_eq!(parse_span("2m").unwrap(), Duration::from_secs(120));
        assert_eq!(parse_span("36h").unwrap(), Duration::from_secs(129_600));
        assert_eq!(parse_span("3d").unwrap(), Duration::from_secs(259_200));
        assert_eq!(parse_span("15").unwrap(), Duration::from_secs(15));
        assert!(parse_span("2w").is_err());
        assert!(parse_span("soon").is_err());
    }

    #[test]
    fn an_expiry_of_zero_is_allowed_but_an_interval_is_at_least_a_second() {
        assert!(parse_span("0").unwrap().is_zero());
        assert_eq!(parse_interval("0").unwrap(), Duration::from_secs(1));
    }
}
