//! Shell completion, installed where the shell already looks; zsh and
//! PowerShell have no such folder, so `NAME completions` prints theirs.

use clap::CommandFactory;
use clap_complete::Shell;
use std::io::Write;
use std::path::PathBuf;

const OPT_OUT: &str = "SFCC_TOOLS_NO_COMPLETIONS";

pub fn script<C: CommandFactory>(shell: Shell, name: &str) -> String {
    let mut raw = Vec::new();
    clap_complete::generate(shell, &mut C::command(), name, &mut raw);
    let mut script = String::from_utf8_lossy(&raw).into_owned();

    if shell == Shell::Bash {
        // clap_complete names hyphenated branches `a__subcmd__b` but dispatches to `a__b`.
        script = script.replace(&name.replace('-', "__subcmd__"), &name.replace('-', "__"));

        // Git Bash may still complete to `NAME.exe`, which a `NAME` completion never sees.
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

pub fn print<C: CommandFactory>(shell: Shell, name: &str) {
    let _ = std::io::stdout().write_all(script::<C>(shell, name).as_bytes());
}

/// Never fails and never prints: completion is not a reason for a command to go wrong.
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
        // Git Bash sources ~/bash_completion.d; strip_exe makes `sfcc-u` Tab drop `.exe`.
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
