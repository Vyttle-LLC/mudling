# Updates

An installed MUDling updates itself from
[GitHub Releases](https://github.com/Vyttle-LLC/mudling/releases), as
Wiffletree does. The code is in `crates/mudling-desktop/src/update`.

## Which copies update

Only a release build: CI embeds `MUDLING_VERSION` from the version tag, and a
build from source leaves it unset, so it never updates. A release build also
has to be somewhere this user can replace it:

| OS | What's replaced | Needs |
|---|---|---|
| macOS | `MUDling.app` | the folder holding it to be writable, such as `~/Applications`, or `/Applications` for an admin |
| Windows | `MUDling.exe` | its folder to be writable, as the per-user install in `%LOCALAPPDATA%\Programs\MUDling` is |
| Linux | the AppImage | MUDling to run as an AppImage (the runtime sets `APPIMAGE`) from a writable folder; the `.tar.gz` doesn't update itself |

A copy that can't update says why under Settings → Data & sync → Updates.

## When it checks

At launch and then daily, off the UI thread, from
`https://api.github.com/repos/Vyttle-LLC/mudling/releases/latest`, while
"Check for updates" is on (it is unless the player turns it off). Check now
checks at once. A release whose asset for this OS and architecture isn't
attached yet isn't an update.

## The assets

`update/asset.rs` names them, in step with the release workflow:

| OS | Asset |
|---|---|
| macOS | `MUDling-<version>-macos-<arm64\|x86_64>.zip`, holding `MUDling.app` |
| Windows | `MUDling-<version>-windows-<x64\|arm64>.zip`, holding `MUDling-<version>-windows-<arch>\MUDling.exe` (not the installer) |
| Linux | `MUDling-<version>-linux-<x64\|arm64>.AppImage` |

## How a download is checked

Every download must match the SHA-256 `digest` GitHub publishes for the asset.
An asset without a digest is refused before it downloads.

On macOS the unpacked bundle must also pass
`codesign --verify --deep --strict`, and its team identifier must match the
running copy's.

Windows and Linux builds are unsigned for now, so there the digest is the
whole check. When they are signed, the check belongs beside macOS's in
`update::stage`.

## How it goes in

A checked update waits until the app exits, on an ordinary quit or from
Restart to update. MUDling first saves everything and runs sync's session-end
push, as any quit does. Restart while connected asks first; afterwards
MUDling opens the same server again with `--reopen`, and a saved login waits
for the vault as at any launch.

- **macOS**: a small script waits for the app to exit, moves `MUDling.app`
  aside, moves the new one in, and puts the old one back if that fails.
- **Windows**: a running `.exe` can't be overwritten, but it can be renamed. The
  new `MUDling.exe` waits beside the old one as `MUDling.exe.new`; on exit the
  old one becomes `MUDling.exe.old` and the new one takes its name. The next
  launch removes `MUDling.exe.old`.
- **Linux**: the same two renames, for the AppImage.

Downloads wait in the OS's cache folder (on macOS
`~/Library/Caches/com.Vyttle.MUDling/update`). An update never touches the data
folder.

## When something fails

A failed download, check or swap leaves the installed copy as it was, running
and working. Settings says so once, at the top of the sheet, and automatic
checks pass over that release until a newer one comes out, so nothing loops.
Check now tries it again. A swap that fails on exit leaves a note in the cache
folder, which the next launch reads, says once and removes.
