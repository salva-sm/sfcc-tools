mod ci;
mod completions;
mod finding;
mod ledger;
mod local;
mod normalize;
mod notify;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand, ValueHint};
use local::{Local, reachable, say};
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
  log-diff run --state ledger.json --sha 9451cff --build 4821
                                       CI: record a deploy and update the team's ledger
  log-diff notify --report new.json    CI: post what `run --report` found to Teams

The team's ledger comes from --shared or LOG_DIFF_SHARED: a path to a clone of the
ledger repository, or a raw URL (with LOG_DIFF_TOKEN or GITHUB_TOKEN when it is private).";

#[derive(Parser)]
#[command(
    name = "log-diff",
    version,
    about = "Tell the SFCC errors a change introduced from the ones already known",
    after_help = EXAMPLES
)]
struct Cli {
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
        Load it from your shell's profile - in ~/.bashrc:\n\n\
        \x20 eval \"$(log-diff completions bash)\"\n\n\
        or in the PowerShell $PROFILE:\n\n\
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
    /// The team's ledger: a path, or a URL
    #[arg(long, value_name = "PATH|URL", env = "LOG_DIFF_SHARED")]
    shared: Option<String>,
    /// No desktop notification, only the terminal
    #[arg(long)]
    no_desktop: bool,
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
    /// The team's ledger, in a checkout of its repository
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    state: PathBuf,
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
    /// Teams webhook (a Workflows webhook, or a legacy incoming webhook)
    #[arg(
        long,
        value_name = "URL",
        env = "LOG_DIFF_WEBHOOK",
        hide_env_values = true
    )]
    webhook: String,
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
    /// Your own ledger
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    state: Option<PathBuf>,
}

#[tokio::main]
async fn main() {
    match run(Cli::parse()).await {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("log-diff: {error:#}");
            std::process::exit(EXIT_FAILED);
        }
    }
}

async fn run(cli: Cli) -> Result<i32> {
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
                say("nothing new to notify");
                return Ok(0);
            }
            notify::teams(&args.webhook, &report).await?;
            say(&format!("posted {} new signature(s)", report.new.len()));
            Ok(0)
        }
        Command::Completions(args) => {
            completions::print::<Cli>(args.shell, "log-diff");
            Ok(0)
        }
        Command::Ack(args) => {
            let state = args.state.unwrap_or_else(default_state);
            local::acknowledge(&state, &args.ids, args.all)?;
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
        say(&format!(
            "{} unreachable ({reason}) - nothing checked",
            local.config.hostname
        ));
        return Ok(0);
    }

    let team = local.team().await?;
    let outcome = local.pass(&dav, &team, args.baseline).await?;
    if args.baseline {
        say("baseline taken; what the sandbox logged so far is known now");
        return Ok(0);
    }
    local.report(&outcome);

    if args.fail_on_new && !outcome.pending.is_empty() {
        say("`log-diff ack` once they are dealt with, or commit with --no-verify");
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
        state: args.state,
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
        say(&format!(
            "--baseline-days ignored: {} already has a baseline - delete it to start over",
            options.state.display()
        ));
    }
    let outcome = ci::run(&config, &dav, &options).await?;

    if outcome.baseline {
        say(&format!(
            "first run on {}: {} signature(s) learned from the log since {}, nothing reported",
            config.hostname, outcome.known, outcome.baseline_from
        ));
        return Ok(0);
    }
    for item in &outcome.report.new {
        let deploy = item
            .deploy
            .as_ref()
            .map(|deploy| format!(" since {}", deploy.sha))
            .unwrap_or_default();
        println!(
            "{}  x{:<5} {}{deploy}",
            item.id,
            item.count,
            notify::headline(
                &item.label,
                item.exception_class.as_deref(),
                item.location.as_deref()
            )
        );
    }
    say(&format!(
        "{} new, {} known on {}",
        outcome.report.new.len(),
        outcome.known,
        config.hostname
    ));

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
        shared: args.shared.filter(|shared| !shared.trim().is_empty()),
        desktop,
    };
    Ok((local, dav))
}

fn default_state() -> PathBuf {
    ledger::local_dir().join("local-ledger.json")
}

fn parse_interval(raw: &str) -> Result<Duration> {
    let raw = raw.trim();
    let (number, unit) = match raw.find(|c: char| !c.is_ascii_digit()) {
        Some(at) => raw.split_at(at),
        None => (raw, "s"),
    };
    let value: u64 = number
        .parse()
        .with_context(|| format!("{raw:?} is not a duration like 10s or 2m"))?;
    let seconds = match unit {
        "s" => value,
        "m" => value * 60,
        _ => bail!("{raw:?} is not a duration like 10s or 2m"),
    };
    Ok(Duration::from_secs(seconds.max(1)))
}

#[cfg(test)]
mod tests {
    use super::parse_interval;
    use std::time::Duration;

    #[test]
    fn reads_an_interval_in_seconds_or_minutes() {
        assert_eq!(parse_interval("10s").unwrap(), Duration::from_secs(10));
        assert_eq!(parse_interval("2m").unwrap(), Duration::from_secs(120));
        assert_eq!(parse_interval("15").unwrap(), Duration::from_secs(15));
        assert!(parse_interval("10h").is_err());
        assert!(parse_interval("soon").is_err());
    }
}
