//! Shell completion, generated from the command-line definition itself so it
//! never falls behind a new command or flag.

use clap::CommandFactory;
use clap_complete::Shell;
use std::io::Write;

/// Print the completion script for `shell` on stdout.
pub fn print<C: CommandFactory>(shell: Shell, name: &str) {
    let mut script = Vec::new();
    clap_complete::generate(shell, &mut C::command(), name, &mut script);
    let mut script = String::from_utf8_lossy(&script).into_owned();
    if shell == Shell::Bash {
        // clap_complete names the branches of a hyphenated command
        // `a__subcmd__b__subcmd__c` but dispatches to `a__b__subcmd__c`, so
        // nothing past the first word would complete.
        script = script.replace(&name.replace('-', "__subcmd__"), &name.replace('-', "__"));
    }
    // Git Bash completes the command name to `NAME.exe`, and a completion
    // registered for `NAME` alone would then never be consulted.
    if shell == Shell::Bash && cfg!(windows) {
        let function = format!("_{}", name.replace('-', "__"));
        let hyphenated = format!("_{name}");
        let registered = [function, hyphenated]
            .into_iter()
            .find(|function| script.contains(&format!("{function}()")));
        if let Some(function) = registered {
            script.push_str(&format!(
                "complete -F {function} -o bashdefault -o default {name}.exe
"
            ));
        }
    }
    let _ = std::io::stdout().write_all(script.as_bytes());
}
