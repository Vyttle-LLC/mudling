#!/usr/bin/env bash
# Compares the gpui-pre crates in the Cargo registry with the Zed sources
# they say they were snapshotted from.
#
#   scripts/verify-gpui-pre.sh [version]    # default: the version Cargo.lock pins
#
# Each gpui-pre crate names its Zed crate and revision in its Cargo.toml
# (`zed-crate`, `zed-rev`). The script downloads Zed at that revision into a
# temporary directory and diffs each crate's sources against Zed's. Files
# that differ, or exist only in gpui-pre, are listed; run it before every
# gpui upgrade and read what it lists.
set -euo pipefail

cd "$(dirname "$0")/.."
version="${1:-$(grep -A1 '^name = "gpui-pre"$' Cargo.lock | sed -n 's/^version = "\(.*\)"/\1/p')}"
cargo fetch --quiet

registry="$(ls -d "${CARGO_HOME:-$HOME/.cargo}"/registry/src/*/ | head -n 1)"
crates=("$registry"gpui-pre*-"$version")
[[ -d "${crates[0]}" ]] || { echo "gpui-pre $version isn't in $registry" >&2; exit 2; }
rev="$(sed -n 's/^zed-rev = "\(.*\)"/\1/p' "${crates[0]}/Cargo.toml")"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
echo "Downloading zed@$rev …"
curl -sSfL "https://codeload.github.com/zed-industries/zed/tar.gz/$rev" | tar -xz -C "$work" --strip-components 1

# Packaging files differ by design; only sources are compared.
excludes=(--exclude Cargo.toml --exclude Cargo.toml.orig --exclude Cargo.lock --exclude '.cargo*'
    --exclude 'LICENSE*' --exclude 'README*')
identical=0
changed=0
for crate in "${crates[@]}"; do
    zed_name="$(sed -n 's/^zed-crate = "\(.*\)"/\1/p' "$crate/Cargo.toml")"
    manifest="$(grep -l "^name = \"$zed_name\"$" "$work"/crates/*/Cargo.toml "$work"/crates/*/*/Cargo.toml 2>/dev/null | head -n 1 || true)"
    zed_dir="${manifest%/Cargo.toml}"
    if [[ -z "$zed_dir" ]]; then
        echo "?? $(basename "$crate"): no Zed crate named $zed_name"
        changed=$((changed + 1))
        continue
    fi
    # "Only in <zed>" means Zed ships files the package leaves out (tests,
    # examples), which can't change what we build.
    differences="$(diff -rq "${excludes[@]}" "$crate" "$zed_dir" | grep -v "^Only in $zed_dir" || true)"
    if [[ -z "$differences" ]]; then
        identical=$((identical + 1))
    else
        changed=$((changed + 1))
        echo "== $(basename "$crate") vs $zed_name:"
        echo "$differences" | sed "s|$crate/||g; s|$zed_dir/||g; s|Only in $crate:|Only in gpui-pre:|; s/^/   /"
    fi
done
echo "gpui-pre $version vs zed@$rev: $identical crates identical, $changed with differences (listed above)"
