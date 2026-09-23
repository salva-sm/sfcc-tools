# log-diff

Tells the errors a change introduced on an SFCC instance from the ones that were already
there. Every record in the instance log is reduced to a signature, and a signature is only
news when nobody has seen it before.

It runs in two places. On CI, against the shared DEV instance after every deploy, it keeps
the team's ledger and posts what is new to Teams. On a developer's machine, against their
sandbox, it reads that ledger without writing to it and only speaks up about what the team
does not already know.

## Install

One file, from the [latest release](https://github.com/salva-sm/sfcc-tools/releases/latest).
It does not need `sfcc-upload` or anything else from this repository:

```powershell
# Windows
curl -L -o log-diff.exe https://github.com/salva-sm/sfcc-tools/releases/latest/download/log-diff-x86_64-windows.exe
```

```bash
# macOS (Apple silicon) / Linux — swap aarch64 for x86_64 as needed
curl -L https://github.com/salva-sm/sfcc-tools/releases/latest/download/log-diff-aarch64-macos.tar.gz | tar -xz
```

[`tools/install-all.ps1`](../../tools/install-all.ps1) and
[`tools/install-all.sh`](../../tools/install-all.sh) fetch every tool at once.

## Usage

```
log-diff check                    one pass over the sandbox log: report what is new
log-diff check --fail-on-new      the same, exiting 1 while anything is pending
log-diff watch [--interval 10s]   the same pass on a timer
log-diff ack [ID... | --all]      list what is pending, or clear it
log-diff run --state ledger.json [--sha SHA --build N]   CI: update the team's ledger
log-diff notify --report new.json                        CI: post the report to Teams
```

Every command takes `--config` for `dw.json` (the nearest one by default, as everywhere
else here) and `--level` for the log files to read, `error,customerror,fatal` by default.

The team's ledger comes from `--shared` or `LOG_DIFF_SHARED`: a path to a clone of the
ledger repository, or a raw URL. A URL to a private repository is fetched with
`LOG_DIFF_TOKEN` or `GITHUB_TOKEN`; when it cannot be fetched the last copy is used, so an
outage does not turn everything the team knows into news.

### Exit status

| | |
| :-- | :-- |
| `0` | Done, whatever was found. An unreachable sandbox is `0` too: there was nothing to check |
| `1` | `--fail-on-new`, and there is something pending |
| `2` | log-diff could not do its job: no `dw.json`, credentials rejected, a ledger it cannot read |

## What a signature is

A record is its first line and the stack under it. The request dump SFCC appends after the
stack is dropped whole, which is where most of the personal data in an error log lives.

From what is left, the moment, the thread number and the session go. Then, in this order:
timestamps, UUIDs, URLs (the host and the shape of the path stay; the query goes, and so does
any segment that is an id), emails, IP addresses, `key=value` secrets (`dwsid`, `token`,
`password`, `authorization`...), bearer tokens, decimals, numbers of four digits or more,
and any word six long or more that mixes letters and digits — order numbers, product ids.
Short numbers stay: `HTTP 404` means something. Paths stay, since a cartridge may have a
digit in its name.

The signature hashes the level, the scrubbed message and the top eight frames **without
their line numbers**. An unrelated edit higher up a file shifts every line below it, and a
known failure should not turn new because of that. A different function, file or message is
a different signature. The line numbers are still kept in what is shown, where they are the
thing you need.

The ledger keeps one scrubbed example per signature, never raw log lines. Scrubbing is
pattern matching, not understanding: a message that spells out a customer's name in plain
words keeps it. Keep the ledger repository private.

## The ledger

```json
{
  "version": 1,
  "instance": "dev01-eu01-acme.demandware.net",
  "cursor": {
    "taken": "2026-09-22T21:40:00Z",
    "day": "20260922",
    "offsets": { "error-blade1-4-appserver-20260922.log": 481516 }
  },
  "known_signatures": {
    "4cfc684705f583bc": {
      "label": "error",
      "exception_class": "NullPointerException",
      "location": "app_acme/cartridge/scripts/checkout/CheckoutServices.js:214",
      "example": "ERROR PipelineCallServlet|Sites-Acme-Site|Checkout-Begin|PipelineCall custom.checkout [] ...",
      "first_seen": "2026-09-10T09:12:00Z",
      "last_seen": "2026-09-22T21:38:00Z",
      "count": 143,
      "first_deploy_sha": "9451cff"
    }
  },
  "deploy_log": [
    { "sha": "9451cff", "build": 4821, "timestamp": "2026-09-22T21:38:00Z", "new_signatures": ["4cfc684705f583bc"] }
  ]
}
```

The cursor is where the last read ended: the length of each of the day's log files, not a
timestamp. Reading from it fetches only the bytes written since, with a `Range` request, so a
pass every ten seconds costs one `PROPFIND` and next to nothing else. A file the cursor does
not know was opened after it and is read whole; the days between the cursor and today are
read too, so a run after a weekend misses nothing still on the instance.

A newer `version` is refused rather than rewritten without the fields this build does not
know.

The developer's own ledger has the same shape, in `%APPDATA%\log-diff\local-ledger.json` on
Windows and `~/.config/log-diff/local-ledger.json` elsewhere (`--state` to move it). It is
only ever written by `check`, `watch` and `ack`, and the team's is only ever written by
`run`.

## On a developer's machine

`check` reads the sandbox log since its last pass — today's log, the first time — and reports
every signature neither the team's ledger nor yours has. Those become **pending**: they are
listed by every `check` and `watch` until `log-diff ack` clears them or the team's ledger
learns them. A desktop notification goes out the first time each one appears, and only then.

`watch` is the same pass on a timer. `check` needs nothing running beforehand; if `watch` is
running, `check` finds what it already recorded and does not report it again.

`check --baseline` takes everything logged so far as known, for a sandbox that has been
failing for reasons of its own.

Each pending signature is printed the way a compiler prints an error, so an editor's problem
matcher reads it as one:

```
C:\dev\site\cartridges\app_acme\cartridge\scripts\checkout\CheckoutServices.js:214: error: [error] TypeError: Cannot read property "shipments" from null (4cfc684705f583bc)
```

The path is the local file when the checkout has it, `dw.json` otherwise.

### VS Code

[`templates/vscode-tasks.json`](templates/vscode-tasks.json) is a `.vscode/tasks.json` that
starts `log-diff watch` when the folder opens (`runOn: folderOpen`; VS Code asks once whether
the folder may do that) with a problem matcher that puts pending signatures in the
**Problems** panel. Each report re-lists every pending signature between a `log-diff:
checking` line and a summary line, which is what a background problem matcher needs to keep
the panel in sync.

### Zed

[`templates/zed-tasks.json`](templates/zed-tasks.json) is a `.zed/tasks.json` with the same
`log-diff watch`. Zed has no equivalent of `runOn: folderOpen` at the time of writing, so it
is started from the command palette (`task: spawn`) or a key binding.

### The pre-commit hook

[`templates/pre-commit`](templates/pre-commit) goes into `.githooks/pre-commit`. It runs
`log-diff check --fail-on-new` and blocks the commit only on exit status `1`: an unreachable
sandbox (given up on after 15 seconds), a missing `dw.json` or no `log-diff` installed let the
commit through. `git commit --no-verify` skips it once.

## On CI

The ledger lives in a repository of its own. Its workflow is
[`templates/ledger-workflow.yml`](templates/ledger-workflow.yml): it downloads `log-diff`
from the latest release, writes a `dw.json` from secrets outside the checkout, runs
`log-diff run`, commits `ledger.json` and calls `log-diff notify`.

It runs on a `repository_dispatch` that Jenkins sends after the DEV deploy —
[`templates/Jenkinsfile.snippet`](templates/Jenkinsfile.snippet) — and on a schedule in
between.

Several merges can land between two deploys, and a deploy's errors only show up once somebody
uses what it shipped, often after the next deploy has been dispatched. So `run` does not blame
the deploy that triggered it. Each new signature goes to the deploy that was live when it was
**first logged**, and the suspects are the commits between that deploy and the one before it:
`--compare-url` (or `LOG_DIFF_COMPARE_URL`), with `{from}` and `{to}` for the two shas, turns
that into a link on the Teams card. Pass `--at` when the deploy went live noticeably before
the run.

The first run has nothing to compare against, and learns what the instance already logs today
instead of reporting all of it.

`notify` posts an Adaptive Card, which both Teams Workflows webhooks and the older incoming
webhooks accept. The webhook comes from `--webhook` or `LOG_DIFF_WEBHOOK`. A report with
nothing new posts nothing.

## Building

```bash
cargo test -p log-diff
cargo install --path .        # puts log-diff on PATH
```

The log reading itself — the mark and everything since it — is `sfcc_core::logs`, shared with
`sfcc-upload errors`. Neither binary runs the other.
