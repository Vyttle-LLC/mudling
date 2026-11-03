# Releasing MUDling

A release is a version tag on `main`. Pushing it starts
[`.github/workflows/release.yml`](../.github/workflows/release.yml), which runs
all of CI, then builds every platform and attaches the builds to one GitHub
Release.

## Cutting a release

1. Make sure `main` is green and holds what you want to ship.
2. Tag the commit on `main` and push the tag:

   ```sh
   git tag v1.2.3 origin/main
   git push origin v1.2.3
   ```

   The tag must look like `v1.2.3` and point at a commit on `main`, or the
   workflow stops before building anything.
3. Watch the **Release** run under Actions. When it finishes, the release is
   on the [releases page](https://github.com/Vyttle-LLC/mudling/releases)
   with notes generated from the merged pull requests.

To write the notes yourself, draft a release for the tag in GitHub's UI first.
The workflow then attaches the builds to that draft instead of creating a
release, and you publish it when you're ready.

## What a release holds

| Asset | What it is |
|---|---|
| `MUDling-<version>-macos-arm64.zip` | `MUDling.app` for Apple Silicon, signed with Developer ID and notarized |
| `MUDling-<version>-macos-x86_64.zip` | The same for Intel Macs |
| `MUDling-<version>-windows-<arch>-setup.exe` | The Windows installer, x64 or arm64: per user, no admin, with a Start menu entry and an uninstaller |
| `MUDling-<version>-windows-<arch>.zip` | `MUDling.exe` alone, to run from any folder |
| `MUDling-<version>-linux-<arch>.AppImage` | One file that runs on most distributions, x64 or arm64 |
| `MUDling-<version>-linux-<arch>.tar.gz` | The binary, its desktop entry and icon |

GitHub publishes each asset's SHA-256 digest, and self-update checks a
download against it. Never rename or replace an asset by hand after upload.

Release builds embed `MUDLING_VERSION`, the tag without its `v`. Settings shows
it; a local build leaves it unset and shows the workspace version with `dev`.

The Linux builds come from Ubuntu 22.04, so they need glibc 2.35 or newer, and
the desktop libraries that `scripts/install-linux-deps.sh` lists (X11 or
Wayland, xkbcommon, fontconfig, freetype and ALSA). Most desktops have them.

## Secrets to add

Add these under **Settings → Secrets and variables → Actions** in
`Vyttle-LLC/mudling`. They are the same names Wiffletree uses, so the same
values work.

| Secret | What it holds |
|---|---|
| `MACOS_CERTIFICATE` | Base64 of the Developer ID Application certificate and its key, as a `.p12` |
| `MACOS_CERTIFICATE_PASSWORD` | The `.p12`'s password |
| `APPLE_API_KEY` | Base64 of an App Store Connect API key (`.p8`) with Developer access |
| `APPLE_API_KEY_ID`, `APPLE_API_ISSUER` | That key's ID and issuer ID |

Without all five, the macOS jobs fail and nothing is published: an unsigned
Mac build would only meet Gatekeeper's refusal.

Windows signing is still to be decided. The workflow signs the app, the
installer and its uninstaller when these exist, and otherwise publishes the
Windows builds unsigned, so SmartScreen warns on first run:

| Secret | What it holds |
|---|---|
| `WINDOWS_CERTIFICATE` | Base64 of a code-signing certificate and its key, as a `.pfx` |
| `WINDOWS_CERTIFICATE_PASSWORD` | The `.pfx`'s password |

A cloud signing service, such as Azure Trusted Signing, would replace the
**Prepare signing** step and the `signtool` calls in
`scripts/package-windows.ps1`.

## Building packages locally

| Platform | Command | Output |
|---|---|---|
| macOS | `scripts/package-macos.sh` | `target/MUDling.app`, ad-hoc signed for this Mac |
| Linux | `cargo build --release -p mudling-desktop && scripts/package-linux.sh` | `target/dist/`, needs `appimagetool` |
| Windows | `cargo build --release -p mudling-desktop --target x86_64-pc-windows-msvc`, then `scripts\package-windows.ps1 -Target x86_64-pc-windows-msvc -Arch x64` | `target\dist\`, needs Inno Setup 6 |

The Windows installer is built with [Inno Setup](https://jrsoftware.org/isinfo.php)
from `scripts/mudling.iss`. It installs per user without admin rights, writes a
Start menu entry and an uninstaller, closes a running copy before it upgrades,
and builds for x64 and arm64 from one script. It is already on GitHub's Windows
runners. WiX builds MSIs, which suit managed fleets but make a per-user install
awkward; NSIS needs more script for the same result.

Installing or reinstalling never touches the data folder: on macOS
`~/Library/Application Support/com.Vyttle.MUDling`, wherever the app itself
lives.
