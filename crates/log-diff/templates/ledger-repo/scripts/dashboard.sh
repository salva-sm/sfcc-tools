#!/usr/bin/env bash
# Rebuild the dashboard from what is committed, and open it.
#
#   scripts/dashboard.sh
#
# The workflow keeps dashboard/index.html up to date on every run; this is for
# a fresh one right after `git pull`, with the links to the SFCC repository.
set -euo pipefail
cd "$(dirname "$0")/.."
git pull --quiet || true
log-diff dashboard --ledger dev=ledgers/dev.json --ledger stg=ledgers/stg.json \
  --ledger prd=ledgers/prd.json --team team.json --out dashboard/index.html \
  --code-url "${CODE_URL:-https://github.com/<owner>/<sfcc-repo>/blob/{sha}/source/cartridges/{path}#L{line}}" \
  --compare-url "${COMPARE_URL:-https://github.com/<owner>/<sfcc-repo>/compare/{from}...{to}}" \
  --open
