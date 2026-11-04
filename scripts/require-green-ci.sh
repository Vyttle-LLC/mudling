#!/bin/sh
# Fails unless CI has passed on main for the given commit, so a release builds
# only what CI has already checked. Needs gh, jq and GH_REPO (owner/name).
# Usage: scripts/require-green-ci.sh <sha>
set -eu

sha="$1"

runs="$(gh run list --workflow ci.yml --event push --branch main --commit "$sha" \
    --json status,conclusion --limit 100)"

if [ "$(printf '%s' "$runs" | jq '[.[] | select(.status == "completed" and .conclusion == "success")] | length')" -gt 0 ]; then
    echo "CI passed on main for $sha."
    exit 0
fi

echo "::error::CI hasn't passed on main for $sha. Wait for it to pass (or re-run it), then delete and push the tag again."
exit 1
