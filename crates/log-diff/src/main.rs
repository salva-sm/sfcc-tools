mod ci;
mod envs;
mod finding;
mod jira;
mod ledger;
mod local;
mod normalize;
mod notify;
mod output;
mod summary;
mod team;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand, ValueHint};
use ledger::Standing;
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
  log-diff list --muted --baseline     what is never reported, most important first
  log-diff unmute <id>                 hear of a muted one again
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
    /// CI: record a deploy in the team's ledger without reading the log
    #[command(
        long_about = "Record a deploy in the team's ledger, without reading the log.\n\n\
        For deploys learned about after the fact - from another workflow's runs, say - so \
        that the next `run` lays each new signature at the right one. A sha already recorded \
        is left alone, so the same list can be fed in on every run."
    )]
    Deploy(DeployArgs),
    /// List the instance's code versions and when each was last written, oldest first
    #[command(
        long_about = "List the code versions on the instance in dw.json, one per line - \
        name, a tab, and when it was last written (RFC 3339) - oldest first. Read-only.\n\n\
        A pipeline that deploys each build to a code version of its own leaves a list of its \
        deploys there, which a workflow can feed to `log-diff deploy`."
    )]
    CodeVersions(InstanceArgs),
    /// CI: post a report written by `run --report` to a Teams channel
    Notify(NotifyArgs),
    /// The last days in errors, per environment: new, most logged, growing
    #[command(
        long_about = "Summarise the last --days days of each environment's ledger: records \
        against the days before, the share that shows as an error page, the new signatures, the \
        most logged and the fastest growing. Printed, and posted to Teams when a webhook is \
        given - a weekly schedule makes it Monday's digest."
    )]
    Summary(SummaryArgs),
    /// Open a Jira ticket for a signature, and remember it in the team file
    #[command(
        long_about = "Open a Jira ticket for a signature, carrying what the ledger knows - \
        where it fails, how often, since which deploy, and the scrubbed example - and record \
        its key in the team file, so the digest - and whatever else reads it - links to it. Jira Cloud, \
        with JIRA_URL, JIRA_EMAIL and JIRA_API_TOKEN from the environment."
    )]
    Ticket(TicketArgs),
    /// List pending signatures, or mark them as dealt with
    Ack(AckArgs),
    /// List signatures by standing - pending, resolved, muted, baseline - most important first
    #[command(
        long_about = "List the signatures in your own ledger by standing, the most important \
        first: what shows as an error page - an uncaught error or a fatal, which SFCC answers \
        with a 500, or a message that says 500 - then what happened most.\n\n\
        With no flag every standing is listed; each flag narrows it down, and they combine."
    )]
    List(ListArgs),
    /// Hear of muted signatures again, or start watching baseline ones
    Unmute(UnmuteArgs),
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
    /// Link to a line of code, with {sha}, {path} (cartridge-relative) and {line}
    #[arg(long, value_name = "URL", env = "LOG_DIFF_CODE_URL")]
    code_url: Option<String>,
    /// The team file: signatures muted for everyone are never reported
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    team: Option<PathBuf>,
    /// Fewest records in a day that can make a spike
    #[arg(long, value_name = "N", default_value_t = 20)]
    spike_min: u64,
    /// Times its usual day a known signature must be logged to spike
    #[arg(long, value_name = "X", default_value_t = 5.0)]
    spike_factor: f64,
    /// Call the instance this in reports - dev, stg, prd - rather than by its host
    #[arg(long, value_name = "NAME")]
    environment: Option<String>,
}

#[derive(Args)]
struct SummaryArgs {
    /// An environment's ledger, `name=path` or `name=url`; repeat for each
    #[arg(
        long = "ledger",
        value_name = "NAME=PATH",
        required_unless_present = "from"
    )]
    ledgers: Vec<String>,
    /// Read every ledger and the team file from a ledger repository on GitHub instead
    #[arg(long, value_name = "OWNER/REPO[@BRANCH]", conflicts_with_all = ["ledgers", "team"])]
    from: Option<String>,
    /// The team file, to leave muted signatures out and name tickets
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    team: Option<PathBuf>,
    /// Days summarised, compared with as many days before them
    #[arg(long, value_name = "N", default_value_t = 7)]
    days: i64,
    /// Teams webhook to post it to; without one it is only printed
    #[arg(
        long,
        value_name = "URL",
        env = "LOG_DIFF_WEBHOOK",
        hide_env_values = true
    )]
    webhook: Option<String>,
    /// Where the dashboard can be opened, for a button on the card
    #[arg(long, value_name = "URL", env = "LOG_DIFF_DASHBOARD_URL")]
    dashboard_url: Option<String>,
}

#[derive(Args)]
struct TicketArgs {
    /// The signature id, or the start of it
    #[arg(value_name = "ID")]
    id: String,
    /// The ledger the signature is in, `name=path`
    #[arg(long, value_name = "NAME=PATH")]
    ledger: String,
    /// The team file the ticket is recorded in
    #[arg(long, value_name = "PATH", default_value = "team.json", value_hint = ValueHint::FilePath)]
    team: PathBuf,
    /// The Jira project key
    #[arg(long, value_name = "KEY", env = "JIRA_PROJECT")]
    project: String,
    /// The issue type
    #[arg(long = "type", value_name = "NAME", default_value = "Bug")]
    kind: String,
    /// Link to a line of code, with {sha}, {path} and {line}
    #[arg(long, value_name = "URL", env = "LOG_DIFF_CODE_URL")]
    code_url: Option<String>,
    /// Print the issue that would be created, and create nothing
    #[arg(long)]
    dry_run: bool,
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

#[derive(Args)]
struct DeployArgs {
    /// The team's ledger (default: the one `run` keeps on this machine)
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    state: Option<PathBuf>,
    /// The commit deployed
    #[arg(long, value_name = "SHA")]
    sha: String,
    /// When it went live, RFC 3339
    #[arg(long, value_name = "TIMESTAMP")]
    at: String,
    /// The CI build that deployed it
    #[arg(long, value_name = "N")]
    build: Option<u64>,
}

#[derive(Args)]
struct ListArgs {
    /// Reported and not dealt with
    #[arg(long)]
    pending: bool,
    /// Acknowledged or expired: reported again if logged again
    #[arg(long)]
    resolved: bool,
    /// Muted on purpose: never reported again
    #[arg(long)]
    muted: bool,
    /// Taken in with a baseline: known from the start, never reported
    #[arg(long)]
    baseline: bool,
    /// Signatures shown per standing, 0 for all
    #[arg(long, short = 'n', value_name = "N", default_value_t = 10)]
    limit: usize,
    /// Your own ledger
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    state: Option<PathBuf>,
}

#[derive(Args)]
struct UnmuteArgs {
    /// Signature ids, or the start of them - muted or baseline ones
    #[arg(value_name = "ID", required_unless_present = "all")]
    ids: Vec<String>,
    /// Every muted signature (not the baseline ones)
    #[arg(long, conflicts_with = "ids")]
    all: bool,
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
        Command::CodeVersions(args) => {
            let config = Config::load(args.config, None)?;
            let dav = Dav::new(&config)?;
            let mut versions: Vec<(String, String)> = dav
                .list(dav.root_url())
                .await?
                .into_iter()
                .filter(|entry| entry.is_dir)
                .filter_map(|entry| {
                    let at = chrono::DateTime::parse_from_rfc2822(&entry.modified).ok()?;
                    let at = at
                        .with_timezone(&chrono::Utc)
                        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
                    Some((at, entry.name))
                })
                .collect();
            versions.sort();
            for (at, name) in versions {
                println!("{name}\t{at}");
            }
            Ok(0)
        }
        Command::Deploy(args) => {
            let state = args.state.unwrap_or_else(team_on_this_machine);
            let mut ledger = ledger::Ledger::load(&state)?;
            let sha = args.sha.trim();
            if ledger.has_deploy(sha) || args.build.is_some_and(|build| ledger.has_build(build)) {
                status(
                    Tone::Info,
                    &format!("deploy {} already recorded", short(sha)),
                );
                return Ok(0);
            }
            let at = chrono::DateTime::parse_from_rfc3339(&args.at)
                .with_context(|| format!("--at {:?} is not an RFC 3339 timestamp", args.at))?
                .with_timezone(&chrono::Utc);
            ledger.record_deploy(sha, args.build, at);
            ledger.save(&state)?;
            status(
                Tone::Ok,
                &format!("deploy {} recorded at {}", short(sha), args.at),
            );
            Ok(0)
        }
        Command::Summary(args) => {
            let (environments, team) =
                ledgers(&args.from, &args.ledgers, args.team.as_deref()).await?;
            let weeks = summary::weeks(&environments, &team, args.days);
            for week in &weeks {
                status(
                    Tone::Info,
                    &format!(
                        "{} · {} records ({}) · {:.0}% error pages · {} new · {} spikes",
                        week.name.to_uppercase(),
                        week.total,
                        summary::trend(week.total, week.before),
                        week.serious_share * 100.0,
                        week.new.len(),
                        week.spikes
                    ),
                );
                for (title, lines) in [
                    ("new", &week.new),
                    ("most logged", &week.top),
                    ("growing", &week.growing),
                ] {
                    for line in lines {
                        println!(
                            "   {title:<12} {}{} x{} ({})",
                            if line.serious { "500 · " } else { "" },
                            line.headline,
                            line.count,
                            summary::trend(line.count, line.before)
                        );
                    }
                }
            }
            if let Some(webhook) = args.webhook.filter(|webhook| !webhook.trim().is_empty()) {
                let card = summary::card(&weeks, args.days, args.dashboard_url.as_deref());
                notify::post(&webhook, &card).await?;
                status(Tone::Ok, "summary posted to Teams");
            }
            Ok(0)
        }
        Command::Ticket(args) => ticket(args).await,
        Command::Notify(args) => {
            let report = notify::Report::load(&args.report)?;
            if report.is_empty() {
                status(Tone::Ok, "nothing new to notify");
                return Ok(0);
            }
            let what = format!(
                "{} new signature(s), {} spike(s)",
                report.new.len(),
                report.spikes.len()
            );
            let Some(webhook) = args.webhook.filter(|webhook| !webhook.trim().is_empty()) else {
                status(
                    Tone::Info,
                    &format!("{what}, but no Teams webhook (LOG_DIFF_WEBHOOK) - nothing sent"),
                );
                return Ok(0);
            };
            notify::teams(&webhook, &report).await?;
            status(Tone::Ok, &format!("posted {what} to Teams"));
            Ok(0)
        }
        Command::Completions(args) => {
            sfcc_core::completions::print::<Cli>(args.shell, "log-diff");
            Ok(0)
        }
        Command::List(args) => {
            let wanted: Vec<Standing> = [
                (args.pending, Standing::Pending),
                (args.resolved, Standing::Resolved),
                (args.muted, Standing::Muted),
                (args.baseline, Standing::Baseline),
            ]
            .into_iter()
            .filter_map(|(asked, standing)| asked.then_some(standing))
            .collect();
            let state = args.state.unwrap_or_else(default_state);
            local::list(&state, &wanted, args.limit)?;
            Ok(0)
        }
        Command::Unmute(args) => {
            let state = args.state.unwrap_or_else(default_state);
            local::unmute(&state, &args.ids, args.all)?;
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
        code_url: args.code_url,
        baseline_days: args.baseline_days,
        team: args.team,
        spike_min: args.spike_min,
        spike_factor: args.spike_factor,
        environment: args.environment,
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
            serious: ledger::serious(&item.label, &item.example),
            deploy,
        };
        println!();
        println!("{}", output::card(&card));
    }
    if !outcome.report.spikes.is_empty() {
        println!();
        status(
            Tone::New,
            &format!(
                "{} spike{} - known errors logged far more than usual",
                outcome.report.spikes.len(),
                if outcome.report.spikes.len() == 1 {
                    ""
                } else {
                    "s"
                }
            ),
        );
    }
    for spike in &outcome.report.spikes {
        let card = Card {
            id: &spike.id,
            label: &spike.label,
            exception: spike.exception_class.as_deref(),
            location: spike.location.as_deref(),
            example: &spike.example,
            count: spike.today,
            first_seen: "",
            last_seen: None,
            badge: Badge::Spike,
            serious: spike.serious,
            deploy: Some(format!("usually ~{:.0} a day", spike.usual)),
        };
        println!();
        println!("{}", output::card(&card));
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

/// The ledgers and team file a command was pointed at: a repository on
/// GitHub, or paths and URLs one by one.
async fn ledgers(
    from: &Option<String>,
    specs: &[String],
    team: Option<&std::path::Path>,
) -> Result<(Vec<envs::Environment>, team::Team)> {
    match from {
        Some(repository) => envs::from_repository(repository).await,
        None => Ok((envs::load(specs).await?, team::Team::load_optional(team)?)),
    }
}

async fn ticket(args: TicketArgs) -> Result<i32> {
    let environment = envs::load(std::slice::from_ref(&args.ledger))
        .await?
        .pop()
        .context("no ledger given")?;
    let matches: Vec<(&String, &ledger::Known)> = environment
        .ledger
        .known_signatures
        .iter()
        .filter(|(id, _)| id.starts_with(args.id.as_str()))
        .collect();
    let (id, known) = match matches.as_slice() {
        [one] => *one,
        [] => bail!(
            "no signature {} in the {} ledger",
            args.id,
            environment.name
        ),
        _ => bail!(
            "{} matches {} signatures - give more of the id",
            args.id,
            matches.len()
        ),
    };

    let mut team = team::Team::load(&args.team)?;
    if let Some(ticket) = team.tickets.get(id) {
        status(
            Tone::Info,
            &format!("{id} already has {} - {}", ticket.key, ticket.url),
        );
        return Ok(0);
    }

    let link = ci::code_url(
        args.code_url.as_deref(),
        known.first_deploy_sha.as_deref(),
        known.location.as_deref(),
    );
    if args.dry_run {
        let target = jira::Target {
            url: String::new(),
            email: String::new(),
            token: String::new(),
            project: args.project,
            kind: args.kind,
        };
        let issue = jira::issue(&target, id, &environment.name, known, link.as_deref());
        println!("{}", serde_json::to_string_pretty(&issue)?);
        return Ok(0);
    }
    let target = jira::Target::from_env(args.project, args.kind)?;
    let issue = jira::issue(&target, id, &environment.name, known, link.as_deref());
    let ticket = jira::create(&target, &issue).await?;
    status(
        Tone::Ok,
        &format!("{} created - {}", ticket.key, ticket.url),
    );
    team.tickets.insert(id.clone(), ticket);
    team.save(&args.team)?;
    status(
        Tone::Info,
        &format!(
            "recorded in {} - commit it so everyone sees it",
            args.team.display()
        ),
    );
    Ok(0)
}

/// A commit, shortened; a code version name, when that is all there is, whole.
fn short(sha: &str) -> &str {
    match sha.len() >= 12 && sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        true => &sha[..9],
        false => sha,
    }
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
