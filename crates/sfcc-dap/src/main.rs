//! The `sfcc-dap` executable: a debug adapter an editor speaks to over stdio.

use std::io::{BufReader, IsTerminal, Write};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use sfcc_core::config::Config;
use sfcc_dap::adapter::Adapter;
use sfcc_dap::protocol::{self, Writer};
use sfcc_dap::sdapi::Session;

const USAGE: &str = concat!(
    "sfcc-dap ",
    env!("CARGO_PKG_VERSION"),
    "\n\n",
    "A debug adapter, not a command. An editor starts it and speaks the Debug\n",
    "Adapter Protocol to it over stdin, so running it yourself does nothing.\n\n",
    "  --config <path>           the dw.json to read (default: the nearest one)\n",
    "  --cartridge-path <path>   the cartridges directory (default: from dw.json)\n",
    "  --client-id <id>          reported to the instance (default: sfcc-dap)\n\n",
    "Install the B2C Commerce Debugger extension in Zed instead; it starts this.\n",
    "https://github.com/salva-sm/sfcc-tools"
);

fn main() -> Result<()> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments.iter().any(|it| it == "--version" || it == "-V") {
        println!("sfcc-dap {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    // Run by hand, an adapter looks hung: it is waiting on a handshake nobody
    // is going to type.
    if arguments.iter().any(|it| it == "--help" || it == "-h") || std::io::stdin().is_terminal() {
        println!("{USAGE}");
        return Ok(());
    }

    let config = Config::load(value(&arguments, "--config").map(PathBuf::from), None)
        .context("cannot read dw.json")?;
    let cartridges = value(&arguments, "--cartridge-path")
        .map(PathBuf::from)
        .unwrap_or_else(|| config.cartridges_dir.clone());
    let client_id = value(&arguments, "--client-id").unwrap_or_else(|| "sfcc-dap".to_string());

    let writer = Writer::new(Box::new(std::io::stdout()));
    let session = match Session::open(&config, &client_id) {
        Ok(session) => session,
        Err(error) => {
            // Said on the console as well as returned: an editor reports a
            // failed launch as little more than a shrug otherwise.
            writer.log(format!("cannot attach to {}: {error:#}", config.hostname));
            writer.event("terminated", serde_json::json!({}));
            std::io::stdout().flush().ok();
            bail!(error);
        }
    };

    let adapter = Adapter::new(session, cartridges, writer);
    let mut input = BufReader::new(std::io::stdin());
    while let Some(request) = protocol::read(&mut input)? {
        if !adapter.handle(&request) {
            break;
        }
    }
    Ok(())
}

/// The value after a flag, when it is there.
fn value(arguments: &[String], flag: &str) -> Option<String> {
    let at = arguments.iter().position(|it| it == flag)?;
    arguments.get(at + 1).cloned()
}
