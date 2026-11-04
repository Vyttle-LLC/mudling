#!/bin/sh
# Waits for CI on main to pass for the given commit, so a release publishes
# only what CI has already checked. Passes as soon as any CI run on the commit
# has succeeded, fails at once when the newest run ended otherwise, and gives
# up after CI_WAIT_SECONDS. Needs gh, jq and GH_REPO (owner/name).
# Usage: scripts/require-green-ci.sh <sha>
set -eu

sha="$1"
poll_seconds="${CI_POLL_SECONDS:-30}"
wait_seconds="${CI_WAIT_SECONDS:-5400}"
waited=0

while true; do
    runs="$(gh run list --workflow ci.yml --event push --branch main --commit "$sha" \
        --json status,conclusion,url,createdAt --limit 100 \
        | jq 'sort_by(.createdAt) | reverse')"

    if [ "$(printf '%s' "$runs" | jq '[.[] | select(.status == "completed" and .conclusion == "success")] | length')" -gt 0 ]; then
        echo "CI passed on main for $sha."
        exit 0
    fi

    newest="$(printf '%s' "$runs" | jq -r '.[0] // empty | "\(.status) \(.conclusion) \(.url)"')"
    if [ -z "$newest" ]; then
        echo "CI on main for $sha: no run yet, waiting"
    else
        # shellcheck disable=SC2086 # Three words: status, conclusion, url.
        set -- $newest
        if [ "$1" = completed ]; then
            echo "::error::CI failed on main for $sha ($3). Nothing was published. Fix main, then tag the fixed commit."
            exit 1
        fi
        echo "CI on main for $sha: $(printf '%s' "$1" | tr '_' ' '), waiting"
    fi

    if [ "$waited" -ge "$wait_seconds" ]; then
        echo "::error::CI on main for $sha didn't finish in $((wait_seconds / 60)) minutes. Nothing was published. Once CI is green on main, re-run the failed jobs of this run."
        exit 1
    fi
    sleep "$poll_seconds"
    waited=$((waited + poll_seconds))
done
