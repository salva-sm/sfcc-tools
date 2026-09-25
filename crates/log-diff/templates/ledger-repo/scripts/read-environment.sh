#!/usr/bin/env bash
# Read one environment's log into its ledger: dev, stg or prd.
#
#   scripts/read-environment.sh dev
#
# Everything comes from the environment the workflow sets up; nothing is
# required but the instance's host and credentials, and an environment
# without a host is skipped rather than failed, so a repository can start
# with DEV alone. See README.md for every variable.
set -euo pipefail

env="$1"
ENV="${env^^}"
var() { local name="$1"; printf '%s' "${!name:-}"; }

host="$(var "SFCC_${ENV}_HOSTNAME")"
if [ -z "$host" ]; then
  echo "::notice::$ENV skipped: no SFCC_${ENV}_HOSTNAME secret"
  exit 0
fi

ledger="ledgers/$env.json"
work="$RUNNER_TEMP/sfcc-$env"
mkdir -p "$work/cartridges"

# dw.json outside the checkout, so it can never be committed. It needs a
# cartridges folder next to it even though nothing is uploaded.
jq -n \
  --arg hostname "$host" \
  --arg username "$(var "SFCC_${ENV}_USERNAME")" \
  --arg password "$(var "SFCC_${ENV}_PASSWORD")" \
  --arg id "$(var "SFCC_${ENV}_CLIENT_ID")" \
  --arg secret "$(var "SFCC_${ENV}_CLIENT_SECRET")" \
  --arg self_signed "$(var "SFCC_${ENV}_SELF_SIGNED")" \
  '{hostname: $hostname}
   + (if $username != "" then {username: $username, password: $password} else {} end)
   + (if $id != "" then {"client-id": $id, "client-secret": $secret} else {} end)
   + (if $self_signed == "true" then {"self-signed": true} else {} end)' \
  > "$work/dw.json"

# --- Deploys ------------------------------------------------------------------
# Every build is deployed to a code version of its own, named after it, so the
# instance lists its own deploys: one code version each, written when it went
# up. Nothing in the pipeline has to tell this repository anything.
pattern="$(var "${ENV}_CODE_VERSION_PATTERN")"
pattern="${pattern:-${CODE_VERSION_PATTERN:-}}"
if [ -n "$pattern" ]; then
  log-diff code-versions --config "$work/dw.json" |
  while IFS=$'\t' read -r name at; do
    [[ "$name" =~ $pattern ]] || continue
    build="${BASH_REMATCH[1]}"
    sha=""
    # STG and PRD get builds DEV already had: the same build, the same commit.
    if [ "$env" != "dev" ] && [ -f ledgers/dev.json ]; then
      sha="$(jq -r --argjson build "$build" \
        '[.deploy_log[]? | select(.build == $build) | .sha] | first // empty' ledgers/dev.json)"
    fi
    # Otherwise the last commit on the branch before the build went up - right
    # unless something was merged while it was building.
    if [ -z "$sha" ] && [ -n "${SOURCE_REPO:-}" ] && [ -n "${GH_TOKEN:-}" ]; then
      branch="$(var "${ENV}_SOURCE_BRANCH")"
      branch="${branch:-${SOURCE_BRANCH:-develop}}"
      sha="$(gh api "repos/$SOURCE_REPO/commits?sha=$branch&until=$at&per_page=1" \
        --jq '.[0].sha' 2>/dev/null || true)"
    fi
    log-diff deploy --state "$ledger" --sha "${sha:-$name}" --build "$build" --at "$at" --color never
  done
fi

# A deploy announced by the pipeline itself - Jenkins today, GitHub Actions
# tomorrow - with a repository_dispatch: it knows the commit for certain.
args=()
if [ "${DISPATCH_ENVIRONMENT:-dev}" = "$env" ] && [ -n "${DISPATCH_SHA:-}" ]; then
  args+=(--sha "$DISPATCH_SHA")
  [ -n "${DISPATCH_BUILD:-}" ] && args+=(--build "$DISPATCH_BUILD")
  [ -n "${DISPATCH_AT:-}" ] && args+=(--at "$DISPATCH_AT")
fi
if [ -n "${BASELINE_DAYS:-}" ] && { [ "${BASELINE_ENVIRONMENT:-all}" = "all" ] || [ "$BASELINE_ENVIRONMENT" = "$env" ]; }; then
  args+=(--baseline-days "$BASELINE_DAYS")
fi

# --- The log ------------------------------------------------------------------
log-diff run --config "$work/dw.json" --state "$ledger" --environment "$env" \
  --team team.json --report "$RUNNER_TEMP/report-$env.json" \
  --spike-min "${SPIKE_MIN:-20}" --spike-factor "${SPIKE_FACTOR:-5}" \
  "${args[@]}" --color never

# --- Teams ----------------------------------------------------------------------
# PRD can go to a channel of its own; without any webhook nothing is sent.
webhook="$(var "TEAMS_WEBHOOK_${ENV}")"
LOG_DIFF_WEBHOOK="${webhook:-${TEAMS_WEBHOOK:-}}" \
  log-diff notify --report "$RUNNER_TEMP/report-$env.json" --color never
