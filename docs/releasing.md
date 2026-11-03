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
[updates.md](updates.md) says which assets an installed MUDling updates from.

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

One more is optional:

| Secret | What it holds |
|---|---|
| `MACOS_PROVISIONING_PROFILE` | Base64 of a Developer ID provisioning profile for `com.vyttle.mudling` that allows the Keychain access group `34674BKS72.com.vyttle.mudling` |

With it, the app embeds the profile as `Contents/embedded.provisionprofile` and
is signed with `apple/MUDling.entitlements`, so Touch ID can guard Keychain
items. Without it, the app is signed without entitlements and still launches,
but without Touch ID: macOS kills an app at launch that claims an entitlement no
profile allows.

Make the profile in the Apple Developer portal for team `34674BKS72`, as a
Developer ID profile whose App ID is exactly `com.vyttle.mudling`, not a
wildcard. The entitlements claim that App ID
(`com.apple.application-identifier` is `34674BKS72.com.vyttle.mudling`) and
team; a profile for any other App ID gets the app killed at launch.

### Windows: add before a public release

Windows builds ship unsigned for now. Before a public release, add these for
[Azure Trusted Signing](https://learn.microsoft.com/azure/trusted-signing/),
which Microsoft now calls Artifact Signing. The workflow signs the app and the installer once all six exist, and otherwise
publishes unsigned.

| Secret | What it holds |
|---|---|
| `AZURE_TENANT_ID` | The Microsoft Entra tenant of the app registration below |
| `AZURE_CLIENT_ID`, `AZURE_CLIENT_SECRET` | An app registration with the *Certificate Profile Signer* role on the signing account |
| `AZURE_TRUSTED_SIGNING_ENDPOINT` | The account's region endpoint, such as `https://eus.codesigning.azure.net/` |
| `AZURE_TRUSTED_SIGNING_ACCOUNT` | The Trusted Signing account's name |
| `AZURE_TRUSTED_SIGNING_PROFILE` | Its certificate profile's name |

The uninstaller Inno Setup writes stays unsigned; Windows runs it from
Settings without a SmartScreen check.

What an unsigned build shows: a downloaded `-setup.exe` or `MUDling.exe`
opens a blue **Windows protected your PC** box, *"Microsoft Defender
SmartScreen prevented an unrecognized app from starting"*, with the publisher
as *Unknown publisher*. The player must click **More info**, then **Run
anyway**. Edge may also call the download *not commonly downloaded* and ask to
keep it. `install.ps1` avoids both, since files `Invoke-WebRequest` downloads
carry no mark of the web. A signed build names Vyttle LLC as the publisher; the
SmartScreen warning fades as the signature builds reputation.

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

On Windows, a release build is a windowed app, so launching it opens no console
window. Run from a terminal, its messages, such as a usage error, go nowhere;
redirect them (`MUDling.exe --measure 2> err.txt`) or use a debug build, which
keeps its console. `--measure` still works, since its measured run reports
through a pipe.

Installing or reinstalling never touches the data folder: on macOS
`~/Library/Application Support/com.Vyttle.MUDling`, wherever the app itself
lives.
