mod config;
mod daemon;
mod errors;
mod logging;
mod githook;
mod manifest;
mod ocapi;
mod push;
mod reload;
mod scan;
mod tail;
mod watch;
mod webdav;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use config::{Config, Credentials};
use push::{Ctx, PushOptions, human_bytes};
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;
use watch::WatchOptions;
use webdav::Availability;

const DEFAULT_JOBS: usize = 4;

const EXAMPLES: &str = "\
Examples:
  prost push                       upload what changed since the last sync
  prost push --full                start over: replace every cartridge on the sandbox
  prost start                      watch in the background, surviving the editor
  prost activity                   what the background watcher has been doing
  prost logger                     follow the sandbox log, where server errors land
  prost push --cartridge int_analytics --code-version test1

Configuration comes from the nearest dw.json (hostname, credentials, code-version,
cartridge list). Run `prost doctor` when something does not add up.";

#[derive(Parser)]
#[command(
    name = "prost",
    version,
    about = "Upload SFCC cartridges to a sandbox over WebDAV, from any editor",
    after_help = EXAMPLES
)]
struct Cli {
    /// Path to dw.json (default: the nearest one, searching upwards)
    #[arg(long, short = 'c', global = true, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Code version to target, overriding the one in dw.json
    #[arg(long, global = true, value_name = "NAME")]
    code_version: Option<String>,

    /// Parallel uploads
    #[arg(long, short = 'j', global = true, value_name = "N")]
    jobs: Option<usize>,

    /// Restrict the sync to these cartridges, ignoring the list in dw.json
    #[arg(long, global = true, value_name = "NAME")]
    cartridge: Vec<String>,

    /// Allow writing to an instance that is not a developer sandbox (never staging or production)
    #[arg(long, global = true)]
    allow_shared_instance: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Upload everything that changed since the last sync
    #[command(long_about = "Upload everything that changed since the last sync.\n\n\
        Changes are detected against a local manifest (size, mtime and content hash), and \
        sent as batched archives expanded on the sandbox. Files deleted locally are deleted \
        remotely. The sandbox is probed first and waited for if it is asleep.")]
    Push(PushArgs),
    /// Upload on every save, staying in the foreground
    #[command(long_about = "Upload on every save, staying in the foreground until Ctrl-C.\n\n\
        Runs a push first, then uploads saves, deletions and renames as they happen. While \
        the sandbox is unreachable the changes are queued and flushed on recovery.")]
    Watch(WatchArgs),
    /// Run the watcher detached, so closing the editor does not stop it
    #[command(long_about = "Run the watcher detached, so closing the editor or terminal does \
        not stop it.\n\nOne watcher per sandbox and code version. Follow it with `activity -f`, \
        check it with `status`, end it with `stop`.")]
    Start(WatchArgs),
    /// Stop the detached watcher
    Stop,
    /// Show the watcher, the sandbox and the local sync state
    Status,
    /// Show what the background watcher has been doing
    Activity(LogsArgs),
    /// Check dw.json, connectivity, credentials and code version
    Doctor,
    /// Delete the cartridges of the code version on the sandbox
    #[command(long_about = "Delete the cartridges of the code version on the sandbox and reset \
        the local manifest.\n\nOnly the cartridges of this project are removed, not the whole \
        code version. Asks for confirmation unless -y is given.")]
    Clean(CleanArgs),
    /// List the code versions present on the sandbox
    Versions,
    /// List what the code version holds on the sandbox
    Ls(LsArgs),
    /// Follow the sandbox log, where server-side errors show up
    #[command(long_about = "Follow the sandbox log over WebDAV.\n\n\
        Controller, hook and ISML failures never surface at upload time - they happen when the \
        page runs, and only the sandbox log records them. Today's files of the chosen levels are \
        followed from the end, and stack frames are printed as local paths the terminal can open.")]
    Logger(TailArgs),
    /// Report what the sandbox logged since you marked it
    #[command(long_about = "Report what the sandbox logged since you marked it.\n\n\
        `--mark` records how long today's log files are; reproduce whatever you are testing, \
        then run it again with no arguments to see only what your change produced. Repeats of \
        one failure collapse into a single block with a count. Exits 1 when there is something \
        new, so it chains: `prost errors --mark && npm test && prost errors`.")]
    Errors(ErrorsArgs),
    /// Make the code version the active one, through the Data API
    #[command(long_about = "Make a code version the active one on the sandbox.\n\n\
        WebDAV cannot do this, so it goes through the OCAPI Data API and needs an API client in \
        dw.json whose OCAPI Data settings include /code_versions.")]
    Activate(ActivateArgs),
    /// Install a git hook that pushes after a branch switch
    #[command(long_about = "Install a post-checkout git hook that runs `prost push`.\n\n\
        A branch switch changes files behind the watcher's back if it is not running; the hook \
        makes the sandbox follow the branch. It costs nothing when nothing changed.")]
    InstallHook(InstallHookArgs),
    /// Delete a path inside the code version on the sandbox
    #[command(long_about = "Delete one path inside the code version on the sandbox.\n\n\
        For files or folders left behind that no longer exist locally. The local tree is never \
        touched, and the path is dropped from the manifest so the next push re-uploads it if it \
        is still there.")]
    Rm(RmArgs),
}

#[derive(Args)]
struct LsArgs {
    /// Path inside the code version (default: its root)
    #[arg(value_name = "PATH")]
    path: Option<String>,
}

#[derive(Args)]
struct TailArgs {
    /// Log levels to follow, comma separated, or "all"
    #[arg(long, value_name = "LIST", default_value = tail::DEFAULT_LEVELS)]
    level: String,
    /// Lines of history to print on start
    #[arg(long, short = 'n', value_name = "N", default_value_t = 0)]
    lines: usize,
    /// Seconds between polls
    #[arg(long, value_name = "SECONDS", default_value_t = 3)]
    interval: u64,
    /// Colour the output: auto, always, never
    #[arg(long, value_name = "WHEN", default_value = "auto")]
    color: String,
}

#[derive(Args)]
struct ErrorsArgs {
    /// Record the current end of the log instead of reporting
    #[arg(long)]
    mark: bool,
    /// Log levels to look at, comma separated, or "all"
    #[arg(long, value_name = "LIST", default_value = tail::DEFAULT_LEVELS)]
    level: String,
    /// Colour the output: auto, always, never
    #[arg(long, value_name = "WHEN", default_value = "auto")]
    color: String,
}

#[derive(Args)]
struct RmArgs {
    /// Path inside the code version, e.g. app_common_ui/cartridge/leftovers
    #[arg(value_name = "PATH")]
    path: String,
    /// Do not ask for confirmation
    #[arg(long, short = 'y')]
    yes: bool,
}

#[derive(Args)]
struct PushArgs {
    /// Ignore the local state and re-upload every cartridge from scratch
    #[arg(long)]
    full: bool,
    /// List what would be uploaded without touching the sandbox
    #[arg(long)]
    dry_run: bool,
    /// Make the code version active once the upload finishes
    #[arg(long)]
    activate: bool,
}

#[derive(Args)]
struct ActivateArgs {
    /// Code version to activate (default: the one being synced)
    #[arg(value_name = "NAME")]
    name: Option<String>,
}

#[derive(Args)]
struct InstallHookArgs {
    /// Replace an existing post-checkout hook
    #[arg(long)]
    force: bool,
}

#[derive(Args)]
struct WatchArgs {
    /// Re-upload every cartridge before watching
    #[arg(long)]
    full: bool,
    /// Skip the upload that runs before watching
    #[arg(long)]
    no_initial_push: bool,
    /// Reload the storefront tabs of a Chrome started with --remote-debugging-port
    #[arg(long)]
    reload: bool,
    /// DevTools port to reload through
    #[arg(long, value_name = "PORT", default_value_t = 9222)]
    reload_port: u16,
}

impl WatchArgs {
    fn options(&self) -> WatchOptions {
        WatchOptions {
            initial_push: !self.no_initial_push,
            full: self.full,
            reload_port: self.reload.then_some(self.reload_port),
        }
    }
}

#[derive(Args)]
struct LogsArgs {
    /// Keep printing new lines as they arrive
    #[arg(long, short = 'f')]
    follow: bool,
    /// Lines of history to print
    #[arg(long, short = 'n', default_value_t = 40)]
    lines: usize,
}

#[derive(Args)]
struct CleanArgs {
    /// Do not ask for confirmation
    #[arg(long, short = 'y')]
    yes: bool,
    /// Delete the whole code version folder, not just its cartridges
    #[arg(long)]
    remove_version: bool,
}

#[tokio::main]
async fn main() {
    if let Err(error) = run(Cli::parse()).await {
        logging::error(format!("{error:#}"));
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<()> {
    let mut config = Config::load(cli.config.clone(), cli.code_version.clone())?;
    if !cli.cartridge.is_empty() {
        config.cartridge_filter = Some(cli.cartridge.clone());
    }
    let jobs = cli.jobs.unwrap_or(DEFAULT_JOBS).clamp(1, 16);

    let writes = matches!(
        cli.command,
        Command::Push(_)
            | Command::Watch(_)
            | Command::Start(_)
            | Command::Clean(_)
            | Command::Rm(_)
            | Command::Activate(_)
    );
    if writes {
        config.ensure_writable(cli.allow_shared_instance)?;
    }

    match cli.command {
        Command::Push(args) => {
            let ctx = Ctx::new(config, jobs)?;
            let options = PushOptions { full: args.full, dry_run: args.dry_run, show_progress: true };
            push::push(&ctx, options).await?;
            if args.activate && !args.dry_run {
                activate(&ctx.config, None).await?;
            }
            Ok(())
        }
        Command::Watch(args) => {
            let options = args.options();
            watch::watch(Ctx::new(config, jobs)?, options).await
        }
        Command::Start(args) => {
            let spawn = daemon::SpawnArgs {
                watch: args.options(),
                jobs,
                cartridges: cli.cartridge.clone(),
            };
            start_detached(&config, spawn)
        }
        Command::Stop => stop_detached(&config),
        Command::Status => report_status(config, jobs).await,
        Command::Activity(args) => print_logs(&config, &args),
        Command::Activate(args) => activate(&config, args.name).await,
        Command::InstallHook(args) => {
            let hook = githook::install(&config, args.force)?;
            logging::ok(format!("git hook installed at {}", hook.display()));
            Ok(())
        }
        Command::Doctor => diagnose(config, jobs).await,
        Command::Clean(args) => clean(config, jobs, args).await,
        Command::Versions => list_versions(config, jobs).await,
        Command::Ls(args) => list_remote(config, jobs, args.path).await,
        Command::Rm(args) => remove_remote(config, jobs, args).await,
        Command::Logger(args) => {
            let ctx = Ctx::new(config, jobs)?;
            let options = tail::TailOptions {
                levels: tail::parse_levels(&args.level),
                interval: Duration::from_secs(args.interval.max(1)),
                lines: args.lines,
                color: tail::color_enabled(&args.color),
            };
            tail::follow(&ctx, options).await
        }
        Command::Errors(args) => {
            let ctx = Ctx::new(config, jobs)?;
            let levels = tail::parse_levels(&args.level);
            if args.mark {
                return errors::mark(&ctx, &levels).await;
            }

            let options = errors::ReportOptions { levels, color: tail::color_enabled(&args.color) };
            if errors::report(&ctx, options).await? {
                // Not a failure of the command, so it cannot travel as an Err:
                // it is the answer, and it makes the command chainable.
                std::process::exit(1);
            }
            Ok(())
        }
    }
}

async fn remove_remote(config: Config, jobs: usize, args: RmArgs) -> Result<()> {
    let ctx = Ctx::new(config, jobs)?;
    let path = args.path.trim_matches('/').to_string();

    if !args.yes
        && !confirm(&format!(
            "Delete {}/{path} on {}? [y/N] ",
            ctx.config.code_version, ctx.config.hostname
        ))?
    {
        logging::info("nothing was deleted");
        return Ok(());
    }

    ctx.dav.wait_until_ready(Some(Duration::from_secs(120))).await?;
    if !ctx.dav.delete(&path).await? {
        logging::warn(format!("{path} was not there"));
        return Ok(());
    }

    let mut manifest = manifest::Manifest::load(&ctx.manifest_path);
    manifest.forget(&path);
    manifest.forget_prefix(&path);
    manifest.save(&ctx.manifest_path)?;
    logging::ok(format!("{path} deleted"));
    Ok(())
}

async fn list_remote(config: Config, jobs: usize, path: Option<String>) -> Result<()> {
    let ctx = Ctx::new(config, jobs)?;
    let url = match path {
        Some(ref inside) => ctx.dav.file_url(inside),
        None => ctx.dav.base_url().to_string(),
    };

    for entry in ctx.dav.list(&url).await? {
        let kind = if entry.is_dir { "dir " } else { "file" };
        crate::out!("{kind} {:<46} {}", entry.name, entry.modified);
    }
    Ok(())
}

fn start_detached(config: &Config, spawn: daemon::SpawnArgs) -> Result<()> {
    let pid = daemon::start(config, spawn)?;
    logging::ok(format!("watcher running in the background (pid {pid})"));
    crate::out!("  log:  {}", daemon::log_path(config).display());
    crate::out!("  stop: prost stop");
    Ok(())
}

fn stop_detached(config: &Config) -> Result<()> {
    match daemon::stop(config)? {
        Some(pid) => logging::ok(format!("watcher stopped (pid {pid})")),
        None => logging::info("no watcher was running"),
    }
    Ok(())
}

async fn activate(config: &Config, name: Option<String>) -> Result<()> {
    let target = name.unwrap_or_else(|| config.code_version.clone());
    ocapi::Ocapi::new(config)?.activate(&target).await?;
    logging::ok(format!("{target} is now the active code version on {}", config.hostname));
    Ok(())
}

fn print_logs(config: &Config, args: &LogsArgs) -> Result<()> {
    if args.follow {
        return daemon::follow(config, args.lines);
    }
    crate::out!("{}", daemon::tail(config, args.lines)?);
    Ok(())
}

async fn report_status(config: Config, jobs: usize) -> Result<()> {
    let ctx = Ctx::new(config, jobs)?;
    let tracked = manifest::Manifest::load(&ctx.manifest_path).files.len();

    crate::out!("sandbox      {}", ctx.config.hostname);
    crate::out!("code version {}", ctx.config.code_version);
    crate::out!("cartridges   {}", ctx.config.cartridges_dir.display());
    crate::out!("watcher      {}", daemon::describe_state(&ctx.config));
    crate::out!("log          {}", daemon::log_path(&ctx.config).display());
    crate::out!("tracked      {tracked} file(s) in the local manifest");
    crate::out!("availability {}", describe_availability(&ctx.dav.availability().await));
    Ok(())
}

async fn diagnose(config: Config, jobs: usize) -> Result<()> {
    let ctx = Ctx::new(config, jobs)?;
    let cartridges = scan::cartridge_directories(&ctx.config)?;

    crate::out!("dw.json      {}", ctx.config.dw_json.display());
    crate::out!("sandbox      {} ({:?})", ctx.config.hostname, ctx.config.instance());
    crate::out!("auth         {}", describe_credentials(&ctx.config.credentials));
    crate::out!("code version {}", ctx.config.code_version);
    crate::out!("cartridges   {} in {}", cartridges.len(), ctx.config.cartridges_dir.display());
    crate::out!("target       {}", ctx.dav.base_url());

    let availability = ctx.dav.availability().await;
    crate::out!("availability {}", describe_availability(&availability));

    match availability {
        Availability::Unauthorized => bail!("the sandbox rejected the credentials in dw.json"),
        Availability::Unavailable(reason) => bail!("the sandbox is not reachable: {reason}"),
        Availability::MissingCodeVersion => {
            logging::warn(format!(
                "code version {} does not exist yet - push will create it",
                ctx.config.code_version
            ));
        }
        Availability::Ready => {}
    }

    let files = scan::scan(&ctx.config, &ctx.ignore)?;
    let bytes: u64 = files.iter().map(|file| file.size).sum();
    crate::out!("local        {} file(s), {}", files.len(), human_bytes(bytes));
    logging::ok("configuration looks usable");
    Ok(())
}

async fn clean(config: Config, jobs: usize, args: CleanArgs) -> Result<()> {
    let ctx = Ctx::new(config, jobs)?;
    let names: Vec<String> = scan::cartridge_directories(&ctx.config)?
        .iter()
        .filter_map(|path| path.file_name().map(|name| name.to_string_lossy().into_owned()))
        .collect();

    let target = match args.remove_version {
        true => format!("code version {} entirely", ctx.config.code_version),
        false => format!("{} cartridge folder(s) from {}", names.len(), ctx.config.code_version),
    };
    if !args.yes && !confirm(&format!("Delete {target} on {}? [y/N] ", ctx.config.hostname))? {
        logging::info("nothing was deleted");
        return Ok(());
    }

    ctx.dav.wait_until_ready(Some(Duration::from_secs(120))).await?;
    if args.remove_version {
        ctx.dav.delete_code_version().await?;
        logging::ok(format!("code version {} deleted", ctx.config.code_version));
    } else {
        let deleted = push::delete_paths(&ctx, &names).await?;
        logging::ok(format!("{deleted} cartridge folder(s) deleted"));
    }
    let _ = std::fs::remove_file(&ctx.manifest_path);
    Ok(())
}

async fn list_versions(config: Config, jobs: usize) -> Result<()> {
    let ctx = Ctx::new(config, jobs)?;
    let entries = ctx.dav.list(ctx.dav.root_url().to_string().as_str()).await?;

    for entry in entries.iter().filter(|entry| entry.is_dir) {
        let marker = if entry.name == ctx.config.code_version { "*" } else { " " };
        crate::out!("{marker} {:<28} {}", entry.name, entry.modified);
    }
    Ok(())
}

fn confirm(question: &str) -> Result<bool> {
    crate::outp!("{question}");
    std::io::stdout().flush().ok();
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer).context("cannot read the answer")?;
    Ok(matches!(answer.trim().to_lowercase().as_str(), "y" | "yes"))
}

fn describe_availability(availability: &Availability) -> String {
    match availability {
        Availability::Ready => "ready".to_string(),
        Availability::MissingCodeVersion => "reachable, code version missing".to_string(),
        Availability::Unauthorized => "credentials rejected (HTTP 401/403)".to_string(),
        Availability::Unavailable(reason) => format!("unavailable: {reason}"),
    }
}

fn describe_credentials(credentials: &Credentials) -> String {
    match credentials {
        Credentials::Basic { username, .. } => format!("basic, user {username}"),
        Credentials::OAuth { client_id, .. } => format!("oauth client {client_id}"),
    }
}
