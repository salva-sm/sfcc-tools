<div align="center">

# sfcc-tools

**Editor and command-line tooling for Salesforce B2C Commerce, in Rust.**

[![CI](https://github.com/salva-sm/sfcc-tools/actions/workflows/ci.yml/badge.svg)](https://github.com/salva-sm/sfcc-tools/actions/workflows/ci.yml)
[![Docs](https://github.com/salva-sm/sfcc-tools/actions/workflows/docs.yml/badge.svg)](https://salva-sm.github.io/sfcc-tools/isml_lsp/)
[![Release](https://img.shields.io/github/v/release/salva-sm/sfcc-tools?color=blue)](https://github.com/salva-sm/sfcc-tools/releases/latest)
[![Licence](https://img.shields.io/badge/licence-MIT-blue.svg)](LICENSE)

</div>

> [!NOTE]
> **Built with AI assistance**, under human direction on requirements, architecture
> and review.

An SFCC checkout gives a general-purpose editor nothing to work with. There are no type
definitions for the `dw.*` API, no way to resolve a cartridge-relative `require`, and no
record of which of the four cartridges declaring a route is the one that actually runs.
The loop between saving a file and seeing it on a sandbox goes through a separate
uploader the editor cannot see.

These are the pieces that close those gaps. No instance, no network, no `node_modules`.

## What is here

**Crates** — the Rust programs, one cargo workspace:

| | | |
| :-- | :-- | :-- |
| 🔌 | **[`crates/sfcc-core`](crates/sfcc-core)** | What they all share: the `dw.json` reader, where each tool keeps its state and the files one writes for another, and the WebDAV log reader |
| 📤 | **[`crates/sfcc-upload`](crates/sfcc-upload)** | Cartridge uploader for sandboxes, as a CLI. Same job as Prophet, with no editor attached. Formerly `prost` |
| 🔎 | **[`crates/log-diff`](crates/log-diff)** | Tells the errors a deploy or a change introduced from the ones already known: DEV, STG and PRD for the team, with spikes and a weekly digest; and each sandbox for its developer |
| 🧠 | **[`crates/isml-lsp`](crates/isml-lsp)** | Language server: ISML and `dw.*` completion, metadata-backed checks, route override chains, go-to-definition |
| 🐞 | **[`crates/sfcc-dap`](crates/sfcc-dap)** | Debug adapter for server-side scripts: DAP to the editor, the instance's own debugger API on the other side |
| 🖥️ | **[`crates/sfcc-tui`](crates/sfcc-tui)** | Optional: one terminal screen for the watchers, the errors `log-diff` has pending and the debug sessions |

**Grammar and Zed extensions** — outside the workspace:

| | | |
| :-- | :-- | :-- |
| 🌳 | **[`grammar`](grammar)** | `tree-sitter-isml` — the only tree-sitter grammar for ISML there is |
| ✏️ | **[`extensions/isml`](extensions/isml)** | Zed extension wiring the grammar and `isml-lsp` together |
| 🧩 | **[`extensions/b2c-debug`](extensions/b2c-debug)** | Zed extension registering `sfcc-dap` as a debug adapter |

Each has its own README. This one only says how they fit together.

## Install

**Binaries** — `sfcc-upload`, `log-diff`, `isml-lsp` and `sfcc-dap`, no toolchain needed, each
on its own, and `sfcc-tui` if you want it. Grab them from the [latest release](https://github.com/salva-sm/sfcc-tools/releases/latest):

```powershell
# Windows
curl -L -o sfcc-upload.exe https://github.com/salva-sm/sfcc-tools/releases/latest/download/sfcc-upload-x86_64-windows.exe
curl -L -o log-diff.exe https://github.com/salva-sm/sfcc-tools/releases/latest/download/log-diff-x86_64-windows.exe
```

```bash
# macOS (Apple silicon) / Linux — swap aarch64 for x86_64 as needed
curl -L https://github.com/salva-sm/sfcc-tools/releases/latest/download/sfcc-upload-aarch64-macos.tar.gz | tar -xz
curl -L https://github.com/salva-sm/sfcc-tools/releases/latest/download/log-diff-aarch64-macos.tar.gz | tar -xz
```

Or all of them at once, into one folder: [`tools/install-all.ps1`](tools/install-all.ps1) on
Windows, [`tools/install-all.sh`](tools/install-all.sh) elsewhere.

**Zed extensions** — download `isml-<version>.zip` or `b2c-debug-<version>.zip` from the
same release, unzip anywhere and run the `install.cmd` inside. The ISML extension finds
`isml-lsp` on your `PATH`, or downloads its own copy when it is not there.

**From source:**

```bash
cargo install --path crates/sfcc-upload
cargo install --path crates/log-diff
cargo install --path crates/isml-lsp
cargo install --path crates/sfcc-dap
cargo install --path crates/sfcc-tui     # optional
```

## Suggested tasks

Few, because most of it starts on its own. The watcher starts with the editor, its state
shows in the editor's status, and debugging has its own configuration. What is left to run
by hand is a `push` (in a terminal, where it can ask before overwriting a colleague's
change) and the sandbox log.

**The team's ledger (optional).** `log-diff` compares your sandbox with what the team already
knows when `LOG_DIFF_SHARED` points at the ledger. It can be a raw URL, a clone of the ledger
repository (its `ledgers/dev.json` is read) or a ledger file. Set it once in your user
environment, and nothing below needs it:

```bash
setx LOG_DIFF_SHARED "https://raw.githubusercontent.com/<owner>/sfcc-log-ledger/main/ledgers/dev.json"
```

In VS Code it can also go in the `log-diff` task's `"env"`, per project. A path that is not
there is said on every check. With no ledger at all, `log-diff`
still works and compares only against what you have seen.

**Zed.** Start both watchers with the workspace, in `.zed/settings.json`: the uploader, and
`log-diff`. Both run detached (`sfcc-upload start`, `log-diff start`), outlive Zed, and show
in its status bar:

```json
{
  "lsp": {
    "isml-lsp": {
      "initialization_options": { "upload": { "autostart": true }, "errors": { "autostart": true } }
    }
  }
}
```

and `.zed/tasks.json`, run with `task: spawn`:

```json
[
  { "label": "SFCC: push", "command": "sfcc-upload", "args": ["push"], "allow_concurrent_runs": false },
  { "label": "SFCC: sandbox log", "command": "sfcc-upload", "args": ["logger"], "use_new_terminal": true, "allow_concurrent_runs": false }
]
```

Debugging goes in `.zed/debug.json`, see [`extensions/b2c-debug`](extensions/b2c-debug).

**VS Code.** Both watchers can start when the folder opens, with no terminal shown, and
report into Problems. Merge
[`crates/sfcc-upload/templates/vscode-tasks.json`](crates/sfcc-upload/templates/vscode-tasks.json)
and [`crates/log-diff/templates/vscode-tasks.json`](crates/log-diff/templates/vscode-tasks.json)
into `.vscode/tasks.json`, and add the push:

```json
{ "label": "SFCC: push", "type": "process", "command": "sfcc-upload", "args": ["push"], "problemMatcher": [] }
```

A watcher started by VS Code belongs to the window, and does not run next to a detached one
for the same project. Use the editor's or the `start` one, not both.

## Layout

```
crates/       the Rust programs, one cargo workspace
grammar/      tree-sitter-isml, buildable on its own
extensions/   the Zed extensions, deliberately outside the workspace
tools/        packaging and install scripts, shared by both extensions
```

The extensions sit outside the workspace because Zed compiles an extension in place with
a `--target-dir` it hardcodes inside the extension's own directory; a crate belonging to
a workspace resolves against the workspace root instead.

## Building

```bash
cargo test                        # every crate
cargo build --release             # every binary
cargo doc --no-deps --lib --open  # the language server's reference
```

📖 The language server's API reference is published at
**[salva-sm.github.io/sfcc-tools](https://salva-sm.github.io/sfcc-tools/isml_lsp/)**.

## Releasing

One tag, one release, everything on it: the five binaries for five platforms each, and
both Zed extension zips. The extension can only ask GitHub for the *latest* release, so a
per-component tag would leave it looking for an asset that release does not carry.

```bash
git tag v0.2.0 && git push origin v0.2.0
```

## History

This repository is the three that came before it, merged with their history intact:
[`prost`](https://github.com/salva-sm/prost) (now `sfcc-upload`),
[`sfcc-zed-isml`](https://github.com/salva-sm/sfcc-zed-isml) and
[`sfcc-zed-debugger`](https://github.com/salva-sm/sfcc-zed-debugger). All three are
archived; everything continues here.

## Licence

MIT. The grammar keeps `tree-sitter-html`'s copyright notice in
[`grammar/LICENSE`](grammar/LICENSE).
