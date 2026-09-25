# sfcc-log-ledger

The known errors of an SFCC site on **DEV, STG and PRD**, the workflow that keeps the lists,
and a dashboard to read them.

Every 30 minutes on working days, [`log-diff`](https://github.com/salva-sm/sfcc-tools/tree/main/crates/log-diff)
reads each instance's log since its last read, reduces every record to a signature
(scrubbed of order numbers, emails, ids and tokens) and commits it to that environment's
ledger. Teams hears about three things only:

- **new** — a signature nobody had seen on that environment, with the deploy it showed up
  under and the commits to suspect;
- **spikes** — a known signature logged far more today than on its usual day (at least
  20 times, and 5× its average day of the week before);
- **Monday's digest** — the week per environment: records against the week before, the
  share that shows as an error page, what is new, what is most logged, what is growing.

Nothing in the SFCC repository or in the Jenkins pipeline depends on this repository, or had to
change for it.

## What is here

| | |
| :-- | :-- |
| [`ledgers/dev.json`](ledgers) · `stg.json` · `prd.json` | One per environment. Written by the workflow only |
| [`team.json`](team.json) | What the team decided: signatures muted for everyone, and their tickets. Edited by people, by pull request |
| [`dashboard/index.html`](dashboard) | Rebuilt on every run from the three ledgers |
| [`.github/workflows/log-diff.yml`](.github/workflows/log-diff.yml) | Reads the three environments, commits, tells Teams |
| [`.github/workflows/summary.yml`](.github/workflows/summary.yml) | Monday's digest, 07:00 UTC |
| [`scripts/`](scripts) | What the workflow runs per environment, and a local dashboard |

## The dashboard

One self-contained HTML page — nothing is fetched, so it works from a clone:

```bash
git pull && start dashboard/index.html          # Windows; `open` on macOS
scripts/dashboard.sh                            # or rebuild it, with links to the code
```

Every run also attaches it to the workflow run as the **dashboard** artifact.

It shows, for the last 7, 30 or 90 days and for one environment or all three: records per
day with the deploys marked, new signatures per day, the most important signatures (error
pages first, then the most logged, with their trend and last 14 days), spikes, what reached
PRD after DEV or STG had it first, and the deploys with the new signatures each brought.
Click a signature for its history, its scrubbed example and a link to the line in
the SFCC repository. Links into it: `?env=prd`, `?range=7`, `?signature=<id>`, `?theme=dark`.

## Deploys

Learned from each instance, with nothing to call: Jenkins deploys every build to a code
version of its own, `b<build>_<date>`, and the moment it was written is when the
build went up.

- **DEV** names each build after the last commit on `develop` before it went up — right
  unless something was merged while it was building.
- **STG and PRD** get builds DEV already had, so a build there takes the commit DEV
  recorded for the same build number.

Two ways to make it exact, both optional and neither needed now:

- **From Jenkins**, after a deploy stage, a `repository_dispatch` naming the environment:

  ```bash
  curl -fsS -X POST -H "Authorization: Bearer $GH_TOKEN" -H "Accept: application/vnd.github+json" \
    https://api.github.com/repos/<owner>/sfcc-log-ledger/dispatches \
    -d '{"event_type":"sfcc-deploy","client_payload":{"environment":"dev","sha":"'"$GIT_COMMIT"'","build_number":'"$BUILD_NUMBER"',"deployed_at":"'"$(date -u +%Y-%m-%dT%H:%M:%SZ)"'"}}'
  ```

- **From GitHub Actions**, when the deploys move there: the same dispatch as a last step,
  or nothing at all — the code versions keep telling it, whoever deploys them.

## Setup

**Secrets** (*Settings → Secrets and variables → Actions → Secrets*). An environment
without its `HOSTNAME` is skipped, so this can start with DEV alone:

| Secret | |
| :-- | :-- |
| `SFCC_DEV_HOSTNAME` · `SFCC_STG_HOSTNAME` · `SFCC_PRD_HOSTNAME` | Each instance's host, without `https://` |
| `SFCC_<ENV>_USERNAME`, `SFCC_<ENV>_PASSWORD` | A Business Manager user on that instance and its WebDAV access key (*profile → Access Keys*, scope *WebDAV File Access and UX Studio*) |
| `SFCC_<ENV>_CLIENT_ID`, `SFCC_<ENV>_CLIENT_SECRET` | Instead of the two above: an API client with read access to `/logs` and `/cartridges` in *Administration → Organization → WebDAV Client Permissions* |
| `SOURCE_TOKEN` | Fine-grained token, resource owner `<owner>`, repository `<sfcc-repo>` only, *Contents: read-only* — to name DEV's builds after their commit |
| `TEAMS_WEBHOOK` | Optional. A Teams Workflows webhook for the channel |
| `TEAMS_WEBHOOK_PRD` (and `_DEV`, `_STG`) | Optional. A channel of its own for one environment |

Reading a log never writes to an instance: WebDAV read access is all it needs.

**Variables** (*Settings → Secrets and variables → Actions → Variables*):

| Variable | Value | |
| :-- | :-- | :-- |
| `CODE_VERSION_PATTERN` | `^b([0-9]+)_` | How a build's code version is named; `STG_CODE_VERSION_PATTERN` or `PRD_…` if one differs |
| `SOURCE_REPO` | `<owner>/<sfcc-repo>` | |
| `SOURCE_BRANCH` | `develop` | The branch DEV is deployed from |
| `COMPARE_URL` | `https://github.com/<owner>/<sfcc-repo>/compare/{from}...{to}` | The commits between two deploys |
| `CODE_URL` | `https://github.com/<owner>/<sfcc-repo>/blob/{sha}/source/cartridges/{path}#L{line}` | A signature's line of code |
| `DASHBOARD_URL` | | Optional: where the dashboard is served, for a button on the digest |
| `SPIKE_MIN`, `SPIKE_FACTOR` | `20`, `5` | Optional: what counts as a spike |

**Workflow permissions:** the workflow commits the ledgers, so *Settings → Actions →
General → Workflow permissions* must allow *Read and write* if the organisation defaults to
read-only.

**First run:** *Actions → log-diff → Run workflow*. The first read of an environment learns
instead of reporting — *baseline_days* (14 by default) of its log, the compressed days in
`log_archive` included — so the charts and the spikes start with two weeks of history.

## Muting and tickets

A signature that does not matter goes in `team.json`, by pull request:

```json
{
  "muted": {
    "4cfc684705f583bc": { "reason": "Bot traffic on an old URL", "by": "<you>" }
  },
  "tickets": {}
}
```

It is still counted, but never reported: not as new, not as a spike, not in the digest; the
dashboard hides it unless *Hide muted* is unticked.

A Jira ticket, carrying everything the ledger knows, with its key recorded in `team.json`:

```bash
export JIRA_URL=https://<site>.atlassian.net JIRA_EMAIL=<you> JIRA_API_TOKEN=<token>
log-diff ticket 4cfc --ledger prd=ledgers/prd.json --project <KEY>
git commit -am "Ticket for 4cfc68" && git push
```

`--dry-run` prints the issue without creating it.

## Using it from a sandbox

`log-diff check` and `log-diff watch` leave out what DEV already knows. Point them at it
once, in your environment:

```bash
export LOG_DIFF_SHARED=https://raw.githubusercontent.com/<owner>/sfcc-log-ledger/main/ledgers/dev.json
export GITHUB_TOKEN=<fine-grained token, this repository, Contents: read-only>
```

Without access to this repository they still work, comparing against what you have seen.

## Keep it private

The ledgers hold scrubbed examples of real errors, PRD's included. Scrubbing is pattern
matching: a message that spells out a name in plain words keeps it.
