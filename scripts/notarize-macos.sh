#!/usr/bin/env bash
# Notarizes a signed app, staples the ticket to it and checks that Gatekeeper
# accepts it. Apple's services drop connections now and then, so submitting and
# stapling each retry with a growing pause; a submission Apple rejects fails at
# once, with its notarization log.
#
# Needs APPLE_API_KEY (the base64 .p8), APPLE_API_KEY_ID and APPLE_API_ISSUER.
# NOTARIZE_PAUSES lists the seconds to wait before each retry.
# Usage: scripts/notarize-macos.sh <path to MUDling.app>
set -euo pipefail

app="$1"
read -ra pauses <<< "${NOTARIZE_PAUSES:-10 20 40 60}"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

echo "$APPLE_API_KEY" | base64 --decode > "$work/api-key.p8"
credentials=(--key "$work/api-key.p8" --key-id "$APPLE_API_KEY_ID" --issuer "$APPLE_API_ISSUER")

# Runs a command until it succeeds, pausing between attempts. Exit status 2 from
# the command means retrying cannot help, so it stops there.
retry() {
    local attempt=1 status=0
    while true; do
        "$@" && return 0 || status=$?
        [ "$status" -ne 2 ] || return 2
        if [ "$attempt" -gt "${#pauses[@]}" ]; then
            echo "::error::'$*' failed on all $attempt attempts"
            return 1
        fi
        echo "Attempt $attempt of '$*' failed; retrying in ${pauses[$((attempt - 1))]}s"
        sleep "${pauses[$((attempt - 1))]}"
        attempt=$((attempt + 1))
    done
}

# Status 2 is Apple saying no; any other failure is the network or Apple's service.
submit() {
    local result="$work/submission.json" submission_status
    xcrun notarytool submit "$work/notarize.zip" --wait --output-format json "${credentials[@]}" \
        > "$result" || true
    submission_status="$(jq -r '.status // empty' "$result" 2>/dev/null || true)"
    case "$submission_status" in
        Accepted) return 0 ;;
        Invalid | Rejected)
            echo "::error::Apple rejected the notarization; its log follows"
            xcrun notarytool log "$(jq -r .id "$result")" "${credentials[@]}" || true
            return 2
            ;;
    esac
    cat "$result"
    return 1
}

ditto -c -k --keepParent "$app" "$work/notarize.zip"
retry submit
retry xcrun stapler staple "$app"
spctl --assess --type execute --verbose "$app"
