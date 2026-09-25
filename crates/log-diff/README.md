# log-diff

Tells the errors a change introduced on an SFCC instance from the ones that were already
there. Every record in the instance log is reduced to a signature, and a signature is only
news when nobody has seen it before.

It runs in two places. On CI, against the shared DEV, STG and PRD instances, it keeps one
ledger per environment, learns the deploys from the instances themselves, posts what is new
and what spiked to Teams, sends a weekly digest, and builds a dashboard of it all. On a
developer's machine, against their sandbox, it reads the DEV ledger without writing to it and
only speaks up about what the team does not already know.

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

09:47:23 · `log-diff ack <id>` once fixed, `log-diff ack --mute <id>` if it does not matter
```

Order numbers, emails and ids are scrubbed before anything is shown or kept, which is why
the second message reads `<n>` and `<email>` — and why its two occurrences, for two
different baskets, are one signature. In a terminal the marks are coloured by level (red
for `error`, magenta for `customerror`) and **NEW** is a red badge.

Once it is fixed, or known not to matter:

```console
$ log-diff ack 7d1e
09:52:10 ✔ 1 acknowledged - reported again if logged again
```

Left alone, a pending signature resolves on its own once it has not been logged for three
days; if it is logged again after that, it comes back marked **BACK**.

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
log-diff ack [ID... | --all]      list what is pending, or resolve it; --mute to never hear of it again
log-diff list [--pending --resolved --muted --baseline]   everything, most important first
log-diff unmute <ID... | --all>   hear of a muted signature again
log-diff run [--state ledger.json] [--sha SHA --build N] read a shared instance, update its ledger
             [--baseline-days N] [--team team.json]      first run: learn N days of history
log-diff deploy --sha SHA --at TIMESTAMP                 CI: record a deploy learned elsewhere
log-diff code-versions                                   the instance's code versions, oldest first
log-diff notify --report new.json                        CI: post new errors and spikes to Teams
log-diff summary --ledger dev=... --ledger prd=...       the last days per environment, for Teams
log-diff dashboard --ledger dev=... --ledger prd=...     an HTML dashboard of the ledgers
log-diff ticket <ID> --ledger prd=... --project KEY      a Jira ticket for a signature
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
listed by every `check` and `watch`, and hold the pre-commit hook, until they are dealt with.
A desktop notification goes out when one appears, and only then.

### When a pending signature goes away

| | What happens | Logged again later |
| :-- | :-- | :-- |
| `log-diff ack <id>` | Resolved: fixed | Comes back, marked **BACK**, pending again |
| Not logged for `--expire` (3 days by default) | Resolved on its own | Comes back, marked **BACK**, pending again |
| `log-diff ack --mute <id>` | Muted: it does not matter | Never reported again, until `log-diff unmute <id>` |
| The team's ledger learns it | Known to the team, so not news | Never reported again |

Only pending signatures expire, and the clock runs from the last time one was logged, not
the first: a failure that keeps happening never expires. A **BACK** is pending like any other
— it expires the same way, and comes back again if it returns — so being wrong about a fix
costs one more notification, never a missed one. Whatever a pass reports is shown at least
once, however old its records are. `--expire 0` (or `LOG_DIFF_EXPIRE=0`) keeps everything
pending until it is acknowledged. What `check --baseline` took in behaves as muted.

`log-diff unmute <id>` takes a muted signature back — or a baseline one, to start watching
it — and treats it as resolved: the next time it is logged, it comes back. `--all` unmutes
every muted one, never the baseline.

### Listing

```console
$ log-diff list                       # every standing
$ log-diff list --muted --baseline    # what is never reported
$ log-diff list --pending -n 0        # everything pending, not only the first ten
```

Signatures are grouped by standing — pending, resolved, muted, baseline — and listed most
important first: what shows as an error page comes before anything else, then what happened
most, then what happened last. An error page is an uncaught `error` or a `fatal`, which SFCC
answers with a 500, or any record whose message says `500` or `Internal Server Error`; its
card carries a red **500**. `check` and `watch` order what is pending the same way, after
what is new.

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

The team's ledgers live in a repository of their own, one per environment - DEV, STG and PRD,
or whichever of them the repository is given credentials for. The whole repository is a
template: [`templates/ledger-repo`](templates/ledger-repo) - its workflows, the script that
reads one environment, the team file and a README with every secret and variable. Copy it
into a new private repository and replace `<owner>` and `<sfcc-repo>`.

Every 30 minutes on working days, for each environment in turn, the workflow:

1. records the deploys the instance itself lists (see below);
2. reads the log since the last read - `log-diff run` - into `ledgers/<env>.json`;
3. posts what is new, and what spiked, to Teams;

then rebuilds `dashboard/index.html` from the three ledgers and commits. One environment
failing does not stop the others, and one without credentials is skipped.

### Deploys, without touching the pipeline

When every build is deployed to a code version of its own - `b4378_20260925` and so on -
the instance lists its own deploys. `log-diff code-versions` prints them with when each was
written, which is when that build went up, and `log-diff deploy` records one per build and
skips a build it already has. DEV names each build after the last commit on its branch
before then (right unless something was merged while it was building); STG and PRD get
builds DEV already had, so a build there takes the commit DEV recorded for the same number.

A `repository_dispatch` from the pipeline - Jenkins, or a GitHub Actions deploy workflow -
naming the environment, the sha and the build makes a deploy's commit exact. It is optional:
[`templates/Jenkinsfile.snippet`](templates/Jenkinsfile.snippet) is one. Moving deploys from
one to the other changes nothing here: the code versions keep telling, whoever writes them.

Listing code versions takes WebDAV read access to `/cartridges` as well as `/logs`: a
Business Manager access key has it; an API client needs both in its WebDAV permissions.
Reading never writes to the instance, so a shared DEV, STG or PRD host is fine here even
though `sfcc-upload` refuses to push to one.

### The first run

The first read of an environment has nothing to compare against. It learns instead of
reporting: `--baseline-days` of log before today (the *baseline_days* input when the
workflow is run by hand, 14 unless changed), plus today's. The days the instance has already
compressed into `log_archive` are read too, so asking for two weeks gets two weeks.
Those signatures have no deploy: none was recorded then. The option is ignored once the
ledger has a cursor; delete the ledger to take the baseline again.

### Laying blame

Several merges can land between two deploys, and a deploy's errors only show up once somebody
uses what it shipped, often after the next deploy has been dispatched. So `run` does not blame
the deploy that triggered it. Each new signature goes to the deploy that was live when it was
**first logged**, and the suspects are the commits between that deploy and the one before it:
`--compare-url` (or `LOG_DIFF_COMPARE_URL`), with `{from}` and `{to}` for the two shas, turns
that into a link on the Teams card, and `--code-url` (`LOG_DIFF_CODE_URL`), with `{sha}`,
`{path}` and `{line}`, links the line that failed, at that deploy.

### Spikes

A known signature is news again when it is logged far more than usual - the way a regression
of an old failure looks. The ledger counts every signature per day for the last 90 days;
`run` reports one as a **spike** when today it has been logged at least `--spike-min` times
(20) and `--spike-factor` times (5) its average day over the week before. Each is reported
once a day, however many runs follow, and what first showed up today is new rather than a
spike.

### The team file

`team.json`, next to the ledgers, is what people decided - edited by pull request, never by
the workflow, so a person's edit and a run's commit never touch the same file:

```json
{
  "muted": { "4cfc684705f583bc": { "reason": "Bot traffic on an old URL", "by": "salva" } },
  "tickets": { "7d1e0c42a9b35f16": { "key": "SHOP-123", "url": "https://acme.atlassian.net/browse/SHOP-123" } }
}
```

A muted signature is still counted, but never reported: not as new, not as a spike, not in
the summary. `log-diff ticket <id> --ledger prd=ledgers/prd.json --project KEY` opens a Jira
Cloud ticket carrying where it fails, how often, since which deploy and the scrubbed example
(`JIRA_URL`, `JIRA_EMAIL`, `JIRA_API_TOKEN`), and records its key here; `--dry-run` prints
the issue instead.

### Teams

`notify` posts an Adaptive Card - new signatures with their deploy, commits and code link,
then spikes - which both Teams Workflows webhooks and the older incoming webhooks accept. The
webhook comes from `--webhook` or `LOG_DIFF_WEBHOOK`; without one nothing is posted and the
run still succeeds, and a report with nothing in it posts nothing.

`log-diff summary` is the weekly digest: for each environment, the records of the last
`--days` (7) against the days before, the share that shows as an error page, the new
signatures, the most logged and the fastest growing - printed, and posted when a webhook is
given. The template's `summary.yml` sends it on Monday mornings.

### The dashboard

`log-diff dashboard --ledger dev=ledgers/dev.json --ledger stg=... --ledger prd=...` writes
one self-contained HTML page - data embedded, nothing fetched, so it opens from a clone, a
workflow artifact or Pages. For the last 7, 30 or 90 days, one environment or all of them:

- records per day per environment, with the deploys marked, and new signatures per day;
- the most important signatures - error pages first, then the most logged - with their
  trend against the period before and their last 14 days;
- spikes, what reached PRD after DEV or STG had it first, and the deploys with the new
  signatures each brought;
- a signature's history, scrubbed example, and a link to its line of code.

Each chart has a table view, colour follows the environment (the three validated for colour
blindness together), light and dark are both designed, and `?env=`, `?range=`,
`?signature=` and `?theme=` link into it. `--open` opens it once written.

## In the editor

`check` and `watch` also write how many signatures are pending for the checkout, and the
ISML language server shows it in Zed's status bar - `2 SFCC errors pending (1 new)` - for as
long as something is, the same way it shows the uploader's state.

## Building

```bash
cargo test -p log-diff
cargo install --path .        # puts log-diff on PATH
```

The log reading itself — the mark and everything since it — is `sfcc_core::logs`, shared with
`sfcc-upload errors`. Neither binary runs the other.
