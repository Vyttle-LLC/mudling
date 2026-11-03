#!/usr/bin/env bash
# Builds target/MUDling.app for this Mac's architecture.
#
# Release builds set MUDLING_VERSION, which the app embeds; local builds leave
# it unset and take the workspace's version. CODESIGN_IDENTITY selects a
# Developer ID, and CODESIGN_KEYCHAIN limits its lookup to one keychain; the
# default ad-hoc signature is for local use.
#
# The bundle id names no data folder: MUDling keeps its data where the core's
# `data_folder` says, whatever the bundle is called.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"
version="${MUDLING_VERSION:-$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)}"
identity="${CODESIGN_IDENTITY:--}"
cargo build --locked --release -p mudling-desktop
# Cargo reads a relative CARGO_TARGET_DIR against the working directory, the repository root.
target_dir="${CARGO_TARGET_DIR:-target}"
case "$target_dir" in /*) ;; *) target_dir="$repo_root/$target_dir" ;; esac
app_bundle="$target_dir/MUDling.app"
# Start clean so files from an earlier build, such as a stapled ticket, never get re-signed.
rm -rf "$app_bundle"
mkdir -p "$app_bundle/Contents/MacOS" "$app_bundle/Contents/Resources"
cp "$target_dir/release/mudling-desktop" "$app_bundle/Contents/MacOS/MUDling"

iconset="$target_dir/MUDling.iconset"
rm -rf "$iconset"
mkdir -p "$iconset"
for size in 16 32 128 256 512; do
    for scale in 1 2; do
        pixels=$((size * scale))
        suffix=""
        if [ "$scale" -eq 2 ]; then suffix="@2x"; fi
        sips -z "$pixels" "$pixels" assets/brand/mudling-icon-1024.png \
            --out "$iconset/icon_${size}x${size}${suffix}.png" >/dev/null
    done
done
iconutil -c icns "$iconset" -o "$app_bundle/Contents/Resources/MUDling.icns"

cat > "$app_bundle/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleName</key><string>MUDling</string>
<key>CFBundleDisplayName</key><string>MUDling</string>
<key>CFBundleIdentifier</key><string>com.vyttle.mudling</string>
<key>CFBundleExecutable</key><string>MUDling</string>
<key>CFBundleIconFile</key><string>MUDling.icns</string>
<key>CFBundleVersion</key><string>$version</string>
<key>CFBundleShortVersionString</key><string>$version</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>NSHighResolutionCapable</key><true/>
<key>NSHumanReadableCopyright</key><string>© 2026 Vyttle LLC · MIT License</string>
<key>LSMinimumSystemVersion</key><string>11.0</string>
<key>LSApplicationCategoryType</key><string>public.app-category.role-playing-games</string>
</dict></plist>
PLIST
if [ "$identity" = "-" ]; then
    codesign --force --sign - "$app_bundle"
else
    # Notarization requires the hardened runtime and a timestamp.
    codesign --force --options runtime --timestamp --sign "$identity" \
        ${CODESIGN_KEYCHAIN:+--keychain "$CODESIGN_KEYCHAIN"} "$app_bundle"
fi
printf 'Bundle: %s\n' "$app_bundle"
