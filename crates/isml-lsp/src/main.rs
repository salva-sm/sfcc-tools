//! The `isml-lsp` executable. Everything it does lives in the library;
//! this is the entry point and the message a human gets for running it.

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

/// Run by hand, a language server looks broken: it sits waiting for a
/// handshake on stdin that a person is never going to type. Say so instead.
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
