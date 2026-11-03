#!/usr/bin/env bash
# Installs or replaces MUDling with a published release, the latest by default:
#
#   curl -fsSL https://raw.githubusercontent.com/Vyttle-LLC/mudling/main/scripts/install.sh | bash
#   curl -fsSL .../install.sh | bash -s -- v1.2.3
#
# macOS: MUDling.app in /Applications, or MUDLING_INSTALL_DIR.
# Linux: the AppImage in ~/.local/bin, with a desktop entry.
# MUDLING_RELEASES_URL points at another copy of the releases, for testing.
# MUDling's data folder is never touched.
set -euo pipefail

releases="${MUDLING_RELEASES_URL:-https://github.com/Vyttle-LLC/mudling/releases}"
team_id=34674BKS72

fail() { printf 'mudling: %s\n' "$1" >&2; exit 1; }

# Without a tag, follow the releases/latest redirect to the newest tag.
tag="${1:-$(curl -fsSLI -o /dev/null -w '%{url_effective}' "$releases/latest")}"
tag="${tag##*/}"
[[ "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "No release found (got '$tag')."
version="${tag#v}"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

download() {
    printf 'Downloading MUDling %s…\n' "$version"
    curl -fL --progress-bar -o "$work/$1" "$releases/download/$tag/$1" \
        || fail "No build of $tag for this machine yet. A new release takes a while to publish; try again shortly."
}

install_macos() {
    local install_dir="${MUDLING_INSTALL_DIR:-/Applications}"
    local app="$install_dir/MUDling.app"
    [ -w "$install_dir" ] || fail "Cannot write to $install_dir. Set MUDLING_INSTALL_DIR=~/Applications or rerun with sudo."
    # Under Rosetta uname says x86_64; the hardware says what to fetch.
    local arch=x86_64
    [ "$(sysctl -n hw.optional.arm64 2>/dev/null)" = 1 ] && arch=arm64
    local archive="MUDling-$version-macos-$arch.zip"
    download "$archive"
    ditto -x -k "$work/$archive" "$work"
    # Accept only an intact, Developer ID–signed and notarized bundle.
    codesign --verify --deep --strict "$work/MUDling.app" || fail "The download's signature is broken."
    spctl --assess --type execute "$work/MUDling.app" || fail "The download did not pass Gatekeeper."
    # Gatekeeper accepts any Developer ID; only Vyttle's team signs MUDling.
    local team
    team="$(codesign -dv "$work/MUDling.app" 2>&1 | sed -n 's/^TeamIdentifier=//p')"
    [ "$team" = "$team_id" ] || fail "The download is signed by team '${team:-none}', not Vyttle ($team_id)."

    # Quit only a running copy; asking a closed app to quit would launch it.
    if pgrep -f "$app/Contents/MacOS/" >/dev/null; then
        printf 'Quitting the running MUDling…\n'
        osascript -e 'tell application id "com.vyttle.mudling" to quit'
        while pgrep -f "$app/Contents/MacOS/" >/dev/null; do sleep 0.2; done
    fi
    rm -rf "$app"
    mv "$work/MUDling.app" "$app"
    printf 'Installed MUDling %s in %s\n' "$version" "$install_dir"
    open "$app"
}

install_linux() {
    local arch
    case "$(uname -m)" in
        x86_64) arch=x64 ;;
        aarch64) arch=arm64 ;;
        *) fail "No Linux build for $(uname -m)." ;;
    esac
    local archive="MUDling-$version-linux-$arch.AppImage"
    local bin="$HOME/.local/bin"
    local share="${XDG_DATA_HOME:-$HOME/.local/share}"
    download "$archive"
    chmod +x "$work/$archive"
    # The AppImage unpacks its own icon, without FUSE.
    (cd "$work" && "./$archive" --appimage-extract com.vyttle.mudling.png >/dev/null) \
        || fail "The download is not a working AppImage."

    mkdir -p "$bin" "$share/applications" "$share/icons"
    # A running copy keeps its own file: the new one takes the name beside it.
    mv "$work/$archive" "$bin/MUDling.AppImage"
    cp "$work/squashfs-root/com.vyttle.mudling.png" "$share/icons/com.vyttle.mudling.png"
    cat > "$share/applications/com.vyttle.mudling.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=MUDling
Comment=A touch-first MUD client
Exec="$bin/MUDling.AppImage"
Icon=$share/icons/com.vyttle.mudling.png
Categories=Game;RolePlaying;
Terminal=false
DESKTOP
    printf 'Installed MUDling %s in %s\n' "$version" "$bin/MUDling.AppImage"
}

case "$(uname -s)" in
    Darwin) install_macos ;;
    Linux) install_linux ;;
    *) fail "This script installs on macOS and Linux. On Windows, run scripts/install.ps1 or the installer." ;;
esac
