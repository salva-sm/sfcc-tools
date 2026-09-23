# log-diff

Tells the errors a change introduced on an SFCC instance from the ones that were already
there. Every record in the instance log is reduced to a signature, and a signature is only
news when nobody has seen it before.

It runs in two places. On CI, against the shared DEV instance after every deploy, it keeps
the team's ledger and posts what is new to Teams. On a developer's machine, against their
sandbox, it reads that ledger without writing to it and only speaks up about what the team
does not already know.

## Demo

A `watch` on a sandbox, while a change to checkout is being tried out. Nothing is printed
while nothing changes; the two failures below turned up in one pass, one of them twice.
Everything in it is made up.

```console
$ log-diff watch
09:41:02 · watching sbx-001.dx.commercecloud.salesforce.com every 10s
09:41:03 ✔ sbx-001.dx.commercecloud.salesforce.com · nothing new
09:47:23 ✖ 2 new errors on sbx-001.dx.commercecloud.salesforce.com

 ✖ TypeError  NEW  Checkout-SubmitShipping · error · x1
   Cannot read property "shippingMethod" from undefined
   ↳ app_example/cartridge/scripts/checkout/shippingHelpers.js:88 in selectShippingMethod
   first 09:47 · 7d1e0c42a9b35f16

 ✖ Custom error  NEW  Cart-AddProduct · customerror · x2
   Basket <n> has no default shipment for <email>
   first 09:46 · last 09:47 · c93a51f07e4d28b0

09:47:23 · `log-diff ack <id>` or `log-diff ack --all` once dealt with
```

Order numbers, emails and ids are scrubbed before anything is shown or kept, which is why
the second message reads `<n>` and `<email>` — and why its two occurrences, for two
different baskets, are one signature. In a terminal the marks are coloured by level (red
for `error`, magenta for `customerror`) and **NEW** is a red badge.

Once it is fixed, or known not to matter:

```console
$ log-diff ack 7d1e
09:52:10 ✔ 1 acknowledged
```

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

### Tab completion

Nothing to set up. Every run of `log-diff` — `log-diff --version` is enough — makes sure its
completion script sits where the shell already looks, and rewrites it only when a new
version changes it:

| Shell | Where | |
| :-- | :-- | :-- |
| Git Bash | `~/bash_completion.d/log-diff.bash` | Sourced by every new Git Bash terminal. It also turns on `completion_strip_exe`, so `log-di` Tab completes to `log-diff`, not `log-diff.exe` |
| bash on Linux or macOS | `~/.local/share/bash-completion/completions/log-diff` | Loaded on first use by the bash-completion package |
| fish | `~/.config/fish/completions/log-diff.fish` | Only when fish is set up |

Open a new terminal after the first run. zsh and PowerShell have no such folder, so there
it takes one line in the profile:

```bash
eval "$(log-diff completions zsh)"                               # ~/.zshrc
```

```powershell
log-diff completions powershell | Out-String | Invoke-Expression  # $PROFILE
```

The script comes from the command definitions themselves, so every command, flag and fixed
value completes, and none falls behind. `SFCC_TOOLS_NO_COMPLETIONS=1` stops the files being
written; delete them to remove it.

## Usage

```
log-diff check                    one pass over the sandbox log: report what is new
log-diff check --fail-on-new      the same, exiting 1 while anything is pending
log-diff watch [--interval 10s]   the same pass on a timer
log-diff ack [ID... | --all]      list what is pending, or clear it
log-diff run [--state ledger.json] [--sha SHA --build N] read DEV, update the team's ledger
             [--baseline-days N]                         first run: learn N days of history
log-diff notify --report new.json                        CI: post the report to Teams
log-diff completions <shell>      the tab completion script for zsh or PowerShell
```

Every command takes `--config` for `dw.json` (the nearest one by default, as everywhere
else here) and `--level` for the log files to read, `error,customerror,fatal` by default.

The team's ledger comes from `--shared` or `LOG_DIFF_SHARED`: a path to a clone of the
ledger repository, or a raw URL. A URL to a private repository is fetched with
`LOG_DIFF_TOKEN` or `GITHUB_TOKEN`; when it cannot be fetched the last copy is used, so an
outage does not turn everything the team knows into news.

### Nothing is required

The ledger repository and Teams are both optional, and nothing fails without them:

- **No team ledger anywhere.** `check` and `watch` still work: new means new to you.
- **No repository yet, but DEV to compare against.** Run `log-diff run --config <DEV dw.json>`
  on your own machine. With no `--state` it keeps the team's ledger in
  `log-diff/dev-ledger.json`, next to your own, and `check` and `watch` use it by themselves
  when `LOG_DIFF_SHARED` is not set. Run it again whenever you want it brought up to date.
  When the repository exists, copy that file in as `ledger.json` and CI carries on from
  where it stopped.
- **No Teams webhook.** `notify` says what it would have sent and exits 0.

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

Each signature is printed as a card: what failed, the message, where, and when. The mark and
its colour say the level at a glance — red `✖` for `error`, magenta for `customerror`, `‼` for
`fatal`, yellow `▲` for warnings — and new ones carry a red **NEW** badge:

```
13:05:12 ✖ 1 new error on dev01-eu01-acme.demandware.net

 ✖ TypeError  NEW  Checkout-Begin · error · x3
   Cannot read property "shipments" from null
   ↳ app_acme/cartridge/scripts/checkout/CheckoutServices.js:214 in validateBasket
   first 13:04 · last 13:05 · 4cfc684705f583bc
```

Colour follows the terminal: on when stdout is one and `NO_COLOR` is not set, or as
`--color always|never` says.

`--problems` prints for an editor's problem matcher instead, one line per pending signature
the way a compiler prints an error, with the local file when the checkout has it and
`dw.json` otherwise:

```
C:\dev\site\cartridges\app_acme\cartridge\scripts\checkout\CheckoutServices.js:214: error: [error] TypeError: Cannot read property "shipments" from null (4cfc684705f583bc)
```

### VS Code

[`templates/vscode-tasks.json`](templates/vscode-tasks.json) is a `.vscode/tasks.json` that
starts `log-diff watch --problems` when the folder opens (`runOn: folderOpen`; VS Code asks once whether
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

The ledger lives in a repository of its own, `sfcc-log-ledger` say. Its workflow is
[`templates/ledger-workflow.yml`](templates/ledger-workflow.yml): it downloads `log-diff`
from the latest release of this repository, writes a `dw.json` from secrets outside the
checkout, runs `log-diff run`, commits `ledger.json` and calls `log-diff notify`.

It runs on a `repository_dispatch` that Jenkins sends after the DEV deploy —
[`templates/Jenkinsfile.snippet`](templates/Jenkinsfile.snippet) — on a schedule in
between, and by hand from the Actions tab.

### Setting up the ledger repository

1. **Create it private.** It holds scrubbed error messages from the instance, and nobody
   outside the team needs to read it. It needs no `ledger.json` to start with: the first
   run writes one.
2. **Add the workflow** as `.github/workflows/log-diff.yml`, copied from the template.
   `LOG_DIFF_COMPARE_URL` in it points at the repository the SFCC code lives in, with
   `{from}` and `{to}` for the two shas; it only adds a link to the Teams card, so drop the
   line if that repository has no compare page.
3. **Add the secrets**, under *Settings → Secrets and variables → Actions*:

   | Secret | |
   | :-- | :-- |
   | `SFCC_DEV_HOSTNAME` | The DEV instance host, without `https://` |
   | `SFCC_DEV_USERNAME`, `SFCC_DEV_PASSWORD` | A Business Manager user and its WebDAV access key (*the user's profile → Access Keys*, scope *WebDAV File Access and UX Studio*) |
   | `SFCC_DEV_CLIENT_ID`, `SFCC_DEV_CLIENT_SECRET` | Instead of the two above: an Account Manager API client, given read access to `/logs` in *Administration → Organization → WebDAV Client Permissions* |
   | `TEAMS_WEBHOOK` | A Teams Workflows webhook (or a legacy incoming webhook). Without it, every step but the last one works |

   Reading the log never writes to the instance, so a shared DEV host is fine here even
   though `sfcc-upload` refuses to push to one.
4. **Let the workflow push.** It asks for `contents: write`; an organisation that caps
   Actions at read-only needs *Settings → Actions → Workflow permissions → Read and write*.
5. **Tell Jenkins where to dispatch.** The snippet posts to
   `https://api.github.com/repos/<owner>/sfcc-log-ledger/dispatches` — replace `<owner>` with
   the account or organisation holding the repository. It needs a token stored in Jenkins as
   a secret text credential: a fine-grained token scoped to that one repository with
   *Contents: read and write*, which is what `repository_dispatch` asks for. Put the stage
   right after the DEV deploy, not at the end of the pipeline that goes on to STG and PRD.
6. **Give developers read access**, if they are to use the team's ledger locally.
   `LOG_DIFF_SHARED` is the raw URL of the file,
   `https://raw.githubusercontent.com/<owner>/sfcc-log-ledger/main/ledger.json`, and a private
   repository needs a token in each developer's environment, `GITHUB_TOKEN` or
   `LOG_DIFF_TOKEN` — a fine-grained one with *Contents: read-only* on that repository is
   enough. Without access, `check` and `watch` still work; they just cannot leave out what
   the team already knows.

Nothing else needs to see the repository. `log-diff` itself is downloaded from this one,
which is public, so the workflow needs no token for that.

The first run — by hand from the Actions tab is fine — has nothing to compare against. It
learns every signature in the DEV log, commits them to `ledger.json` and reports nothing.
From the second run on, only signatures missing from the ledger are reported.

How far back the first run learns is `--baseline-days`, the *baseline_days* input when the
workflow is run by hand (14 unless changed): the log of that many days before today, plus
today's. Without it, today's log only, since 00:00 UTC. Days of history are worth having:
a failure that only turns up with a weekly job or a payment method nobody tried today would
otherwise be reported as new the first time it does, and laid at whatever deploy was live.
Only the files still in the instance's `Logs` folder are read — the older ones SFCC moves to
`log_archive`, compressed, are not — so asking for more days than the instance keeps is
harmless, it just reads what there is. Those signatures have no deploy: none was recorded
then. The option is ignored once the ledger has a cursor; delete `ledger.json` to take the
baseline again. A run started by hand or by the schedule records no deploy; only the dispatch from
Jenkins does.

### Laying blame

Several merges can land between two deploys, and a deploy's errors only show up once somebody
uses what it shipped, often after the next deploy has been dispatched. So `run` does not blame
the deploy that triggered it. Each new signature goes to the deploy that was live when it was
**first logged**, and the suspects are the commits between that deploy and the one before it:
`--compare-url` (or `LOG_DIFF_COMPARE_URL`), with `{from}` and `{to}` for the two shas, turns
that into a link on the Teams card. Pass `--at` when the deploy went live noticeably before
the run.

`notify` posts an Adaptive Card, which both Teams Workflows webhooks and the older incoming
webhooks accept. The webhook comes from `--webhook` or `LOG_DIFF_WEBHOOK`; without one,
nothing is posted and the run still succeeds. A report with nothing new posts nothing.

## Building

```bash
cargo test -p log-diff
cargo install --path .        # puts log-diff on PATH
```

The log reading itself — the mark and everything since it — is `sfcc_core::logs`, shared with
`sfcc-upload errors`. Neither binary runs the other.
