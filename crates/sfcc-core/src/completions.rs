//! Tab completion for the command-line tools, generated from their clap
//! definitions so it never falls behind a new command or flag - and set up
//! by the tools themselves, so nobody has to paste anything into a profile.
//!
//! Every run makes sure the script sits where the shell already looks:
//!
//! - Git Bash sources every `~/bash_completion.d/*.bash` in each new terminal.
//!   The script there also turns on `completion_strip_exe`, so `sfcc-u` Tab
//!   completes to `sfcc-upload` rather than `sfcc-upload.exe`.
//! - bash elsewhere, with the bash-completion package, loads
//!   `~/.local/share/bash-completion/completions/NAME` on first use.
//! - fish loads `~/.config/fish/completions/NAME.fish`.
//!
//! The file is only written when its contents change. zsh and PowerShell
//! have no such folder; `NAME completions zsh|powershell` prints their script
//! for the profile. `SFCC_TOOLS_NO_COMPLETIONS=1` turns all of this off.

use clap::CommandFactory;
use clap_complete::Shell;
use std::io::Write;
use std::path::PathBuf;

/// Set when a user wants no completion files written on their behalf.
const OPT_OUT: &str = "SFCC_TOOLS_NO_COMPLETIONS";

/// The completion script of the command `C`, installed as `name`, for `shell`.
pub fn script<C: CommandFactory>(shell: Shell, name: &str) -> String {
    let mut raw = Vec::new();
    clap_complete::generate(shell, &mut C::command(), name, &mut raw);
    let mut script = String::from_utf8_lossy(&raw).into_owned();

    if shell == Shell::Bash {
        // clap_complete names the branches of a hyphenated command
        // `a__subcmd__b__subcmd__c` but dispatches to `a__b__subcmd__c`, so
        // nothing past the first word would complete.
        script = script.replace(&name.replace('-', "__subcmd__"), &name.replace('-', "__"));

        // Git Bash may still complete the name to `NAME.exe`, where a
        // completion registered for `NAME` alone would never be consulted.
        if cfg!(windows) {
            let function = format!("_{}", name.replace('-', "__"));
            if script.contains(&format!("{function}()")) {
                script.push_str(&format!(
                    "complete -F {function} -o bashdefault -o default {name}.exe\n"
                ));
            }
        }
    }
    script
}

/// Print the script for `shell` on stdout.
pub fn print<C: CommandFactory>(shell: Shell, name: &str) {
    let _ = std::io::stdout().write_all(script::<C>(shell, name).as_bytes());
}

/// Put the script where the shells on this machine look for one. Never fails
/// and never says anything: completion is a convenience, not a reason for a
/// command to go wrong.
pub fn install<C: CommandFactory>(name: &str, version: &str) {
    if std::env::var_os(OPT_OUT).is_some() || std::env::var_os("CI").is_some() {
        return;
    }
    let Some(home) = home() else {
        return;
    };
    let header = format!(
        "# Written by {name} {version}, and rewritten when that changes.\n\
         # To stop it, delete this file and set {OPT_OUT}=1.\n"
    );

    let mut targets: Vec<(PathBuf, Shell, &str)> = Vec::new();
    if cfg!(windows) {
        // Stripping `.exe` is a shell option, so it applies to every command -
        // which is what anyone typing in Git Bash wants anyway.
        targets.push((
            home.join("bash_completion.d").join(format!("{name}.bash")),
            Shell::Bash,
            "shopt -s completion_strip_exe 2>/dev/null\n",
        ));
    } else {
        let data = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local").join("share"));
        targets.push((
            data.join("bash-completion").join("completions").join(name),
            Shell::Bash,
            "",
        ));
    }
    let fish = home.join(".config").join("fish");
    if fish.is_dir() {
        targets.push((
            fish.join("completions").join(format!("{name}.fish")),
            Shell::Fish,
            "",
        ));
    }

    for (path, shell, preamble) in targets {
        let contents = format!("{header}{preamble}{}", script::<C>(shell, name));
        if std::fs::read_to_string(&path).is_ok_and(|current| current == contents) {
            continue;
        }
        if let Some(parent) = path.parent()
            && std::fs::create_dir_all(parent).is_err()
        {
            continue;
        }
        let _ = std::fs::write(&path, contents);
    }
}

fn home() -> Option<PathBuf> {
    ["HOME", "USERPROFILE"]
        .into_iter()
        .filter_map(std::env::var_os)
        .map(PathBuf::from)
        .find(|path| path.is_dir())
}
