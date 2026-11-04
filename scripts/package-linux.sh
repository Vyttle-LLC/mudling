#!/usr/bin/env bash
# Packages a built target/release/mudling-desktop for this machine's
# architecture, as target/dist/MUDling-<version>-linux-<arch>.AppImage and
# .tar.gz. Needs appimagetool on the PATH.
#
# Release builds set MUDLING_VERSION; local builds take the workspace's version.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"
version="${MUDLING_VERSION:-$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)}"
case "$(uname -m)" in
    x86_64) arch=x64 tool_arch=x86_64 ;;
    aarch64) arch=arm64 tool_arch=aarch64 ;;
    *) echo "No Linux package for $(uname -m)" >&2; exit 1 ;;
esac
target_dir="${CARGO_TARGET_DIR:-target}"
binary="$target_dir/release/mudling-desktop"
dist="$target_dir/dist"
name="MUDling-$version-linux-$arch"
rm -rf "$dist"
mkdir -p "$dist"

write_desktop_entry() {
    cat > "$1" <<DESKTOP
[Desktop Entry]
Type=Application
Name=MUDling
Comment=A MUD client
Exec=mudling-desktop
Icon=com.vyttle.mudling
Categories=Game;RolePlaying;
Terminal=false
DESKTOP
}

# The plain archive: the binary, its desktop entry and icon, in one folder.
mkdir -p "$dist/$name"
cp "$binary" LICENSE "$dist/$name/"
cp assets/brand/mudling-icon-1024.png "$dist/$name/com.vyttle.mudling.png"
write_desktop_entry "$dist/$name/com.vyttle.mudling.desktop"
tar -C "$dist" -czf "$dist/$name.tar.gz" "$name"

# The AppImage: an AppDir whose AppRun is the binary.
appdir="$dist/MUDling.AppDir"
mkdir -p "$appdir/usr/bin"
cp "$binary" "$appdir/usr/bin/"
ln -s usr/bin/mudling-desktop "$appdir/AppRun"
cp assets/brand/mudling-icon-1024.png "$appdir/com.vyttle.mudling.png"
write_desktop_entry "$appdir/com.vyttle.mudling.desktop"
ARCH="$tool_arch" appimagetool --no-appstream "$appdir" "$dist/$name.AppImage"

rm -rf "${dist:?}/$name" "$appdir"
printf 'Packages: %s\n' "$dist"/*
