use std::error::Error;
use std::io::IsTerminal;

fn main() -> Result<(), Box<dyn Error + Sync + Send>> {
    match greeting() {
        Some(message) => {
            println!("{message}");
            Ok(())
        }
        None => isml_lsp::server::serve(),
    }
}

/// Run by hand, a language server seems to hang waiting on stdin; say so instead.
fn greeting() -> Option<String> {
    const USAGE: &str = concat!(
        "isml-lsp ",
        env!("CARGO_PKG_VERSION"),
        "\n\n",
        "This is a language server, not a command. Zed starts it and speaks LSP to it\n",
        "over stdin, so running it yourself does nothing visible.\n\n",
        "Install the ISML extension in Zed instead; it picks this binary up from PATH,\n",
        "or downloads its own copy when it is not there.\n",
        "https://github.com/salva-sm/sfcc-tools"
    );

    match std::env::args().nth(1).as_deref() {
        Some("--version" | "-V") => Some(format!("isml-lsp {}", env!("CARGO_PKG_VERSION"))),
        Some("--help" | "-h") => Some(USAGE.to_string()),
        // Any other argument is left alone: an editor may pass its own flags.
        _ if std::io::stdin().is_terminal() => Some(USAGE.to_string()),
        _ => None,
    }
}
