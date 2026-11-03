//! Updates from GitHub Releases, as Wiffletree does them.
//!
//! Only a release build updates itself: CI embeds `MUDLING_VERSION`, and a
//! build from source leaves it unset, so a development copy is never
//! replaced. A newer release is downloaded and checked off the UI thread, then
//! staged; it goes in once the app has saved everything and is exiting, on an
//! ordinary quit or from Restart to update. Nothing here touches the data
//! folder: downloads go to the OS's cache folder, and only the app itself is
//! replaced.
//!
//! Every download is checked against the SHA-256 digest GitHub publishes for
//! the asset, and one without a digest is refused. On macOS the new bundle
//! must also pass `codesign --verify --deep --strict` and be signed by the
//! running copy's team. Windows and Linux builds are unsigned for now, so
//! there the digest is the whole check.

mod asset;
mod state;
#[cfg(test)]
mod tests;

pub use state::{UpdateChoices, UpdateStatus, Updates};

use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const LATEST_RELEASE: &str = "https://api.github.com/repos/Vyttle-LLC/mudling/releases/latest";
/// What a swap that failed leaves for the next launch to say: the version on
/// the first line, why on the rest.
const FAILED_SWAP: &str = "update-failed";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    Mac,
    Windows,
    Linux,
}

/// The OS and architecture a release asset is built for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Platform {
    pub os: Os,
    /// As `std::env::consts::ARCH` names it.
    pub arch: &'static str,
}

impl Platform {
    pub fn current() -> Option<Platform> {
        let os = match std::env::consts::OS {
            "macos" => Os::Mac,
            "windows" => Os::Windows,
            "linux" => Os::Linux,
            _ => return None,
        };
        Some(Platform {
            os,
            arch: std::env::consts::ARCH,
        })
    }
}

/// The running copy, which an update replaces: `MUDling.app` on macOS,
/// `MUDling.exe` on Windows and the AppImage on Linux.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Installed {
    pub platform: Platform,
    pub path: PathBuf,
    /// The version running.
    pub version: String,
}

/// Whether this copy can update itself, and if not, why, as the Data & sync
/// row says it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Eligibility {
    Updates(Installed),
    SourceBuild,
    CantReplace(&'static str),
}

const NO_BUILD: &str = "No release is built for this computer, so it doesn't update itself.";
const NOT_A_BUNDLE: &str = "This copy isn't in MUDling.app, so it can't update itself.";
const NOT_AN_APPIMAGE: &str = "Only the AppImage updates itself; this copy isn't one.";
const NOT_WRITABLE: &str = "MUDling can't write to the folder it's in, so it can't update itself.";

/// Where this copy runs from, and whether it may replace itself there.
/// `appimage` is the `APPIMAGE` path the AppImage runtime sets on Linux, and
/// `writable` says whether a folder takes new files.
pub fn eligibility(
    version: Option<&str>,
    platform: Option<Platform>,
    exe: &Path,
    appimage: Option<&Path>,
    writable: impl Fn(&Path) -> bool,
) -> Eligibility {
    let Some(version) = version else {
        return Eligibility::SourceBuild;
    };
    let Some(platform) = platform.filter(|platform| asset::name(version, *platform).is_some())
    else {
        return Eligibility::CantReplace(NO_BUILD);
    };
    let path = match platform.os {
        // …/MUDling.app/Contents/MacOS/mudling-desktop
        Os::Mac => match exe.ancestors().nth(3) {
            Some(bundle) if bundle.file_name() == Some(OsStr::new(asset::MAC_APP)) => {
                bundle.to_owned()
            }
            _ => return Eligibility::CantReplace(NOT_A_BUNDLE),
        },
        Os::Windows => exe.to_owned(),
        Os::Linux => match appimage {
            Some(appimage) => appimage.to_owned(),
            None => return Eligibility::CantReplace(NOT_AN_APPIMAGE),
        },
    };
    if !path.parent().is_some_and(&writable) {
        return Eligibility::CantReplace(NOT_WRITABLE);
    }
    Eligibility::Updates(Installed {
        platform,
        path,
        version: version.to_owned(),
    })
}

/// Whether this running copy updates itself, by [`eligibility`].
pub fn this_copy() -> Eligibility {
    let Ok(exe) = std::env::current_exe() else {
        return Eligibility::CantReplace(NOT_WRITABLE);
    };
    let appimage = std::env::var_os("APPIMAGE").map(PathBuf::from);
    eligibility(
        mudling_core::RELEASE_VERSION,
        Platform::current(),
        &exe,
        appimage.as_deref(),
        writable,
    )
}

/// The OS's cache folder, never the data folder. Downloads wait in its
/// `update` folder.
pub fn cache_folder() -> Option<PathBuf> {
    directories::ProjectDirs::from("com", "Vyttle", "MUDling")
        .map(|dirs| dirs.cache_dir().to_owned())
}

pub fn staging_folder(cache: &Path) -> PathBuf {
    cache.join("update")
}

fn writable(folder: &Path) -> bool {
    let probe = folder.join(".mudling-update-probe");
    let created = fs::File::create(&probe).is_ok();
    let _ = fs::remove_file(probe);
    created
}

/// A program the updater runs, and what it printed.
pub trait Runner {
    /// Runs `program` to its end: its output, or why it failed.
    fn run(&self, program: &Path, args: &[&OsStr]) -> Result<Ran, String>;
}

pub struct Ran {
    pub stdout: Vec<u8>,
    pub stderr: String,
}

/// Where releases come from: GitHub, or a fake in tests.
pub trait Feed {
    /// The latest release's JSON, as the releases API returns it.
    fn latest(&self) -> Result<Release, String>;
    fn download(&self, url: &str, to: &Path) -> Result<(), String>;
}

/// The programs on this machine.
pub struct System;

impl Runner for System {
    fn run(&self, program: &Path, args: &[&OsStr]) -> Result<Ran, String> {
        let output = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .map_err(|error| format!("Couldn't run {}: {error}", program.display()))?;
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        match output.status.success() {
            true => Ok(Ran {
                stdout: output.stdout,
                stderr,
            }),
            false if stderr.is_empty() => Err(format!("{} failed", program.display())),
            false => Err(stderr),
        }
    }
}

/// GitHub's releases API, through curl, which every OS MUDling runs on has.
pub struct GitHub<'a, R: Runner>(pub &'a R);

impl<R: Runner> GitHub<'_, R> {
    fn curl(&self, args: &[&OsStr]) -> Result<Vec<u8>, String> {
        let common = [
            "--silent",
            "--show-error",
            "--location",
            "--proto",
            "=https",
            "--header",
            "Accept: application/vnd.github+json",
        ];
        let args: Vec<&OsStr> = common
            .iter()
            .map(OsStr::new)
            .chain(args.iter().copied())
            .collect();
        Ok(self.0.run(&tool(Os::current(), "curl"), &args)?.stdout)
    }
}

impl<R: Runner> Feed for GitHub<'_, R> {
    fn latest(&self) -> Result<Release, String> {
        let args = [
            "--max-time",
            "30",
            "--write-out",
            "\n%{http_code}",
            LATEST_RELEASE,
        ];
        let response = self
            .curl(&args.map(OsStr::new))
            .map_err(|error| format!("Couldn't reach GitHub: {error}"))?;
        parse_feed(&String::from_utf8_lossy(&response))
    }

    fn download(&self, url: &str, to: &Path) -> Result<(), String> {
        let args = [
            OsStr::new("--fail"),
            OsStr::new("--max-time"),
            OsStr::new("900"),
            OsStr::new("--output"),
            to.as_os_str(),
            OsStr::new(url),
        ];
        self.curl(&args).map(drop)
    }
}

impl Os {
    fn current() -> Os {
        Platform::current().map_or(Os::Linux, |platform| platform.os)
    }
}

/// A system tool by a path nothing in the app's folder can stand in for.
fn tool(os: Os, name: &str) -> PathBuf {
    match os {
        Os::Mac => Path::new("/usr/bin").join(name),
        Os::Windows => {
            let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
            PathBuf::from(root)
                .join("System32")
                .join(format!("{name}.exe"))
        }
        Os::Linux => PathBuf::from(name),
    }
}

#[derive(Deserialize)]
pub struct Release {
    tag_name: String,
    html_url: String,
    body: Option<String>,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    /// `sha256:<hex>`, which GitHub publishes for every uploaded asset.
    digest: Option<String>,
}

/// Reads the feed's response, whose last line is the HTTP status curl adds.
fn parse_feed(response: &str) -> Result<Release, String> {
    let unreadable = "GitHub's answer couldn't be read.";
    let (body, status) = response.rsplit_once('\n').ok_or(unreadable)?;
    match status {
        "200" => serde_json::from_str(body).map_err(|_| unreadable.to_owned()),
        "404" => Err("GitHub has no published release yet.".to_owned()),
        other => Err(format!("GitHub answered with HTTP {other}.")),
    }
}

/// A verified release, waiting for the app to exit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Staged {
    pub version: String,
    /// The lines of the release notes' `## Summary` section.
    pub summary: Vec<String>,
    /// The release's page, which holds the full notes.
    pub page: String,
    /// The new app, ready to take `installed`'s place.
    path: PathBuf,
    installed: Installed,
}

pub enum Checked {
    /// Nothing newer is ready: there's no newer release, its build for this
    /// computer isn't attached yet, or it's the one the player's copy refused.
    Current,
    Ready(Staged),
}

/// Why a check found nothing to offer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    /// The release that couldn't be readied, if one was found.
    pub version: Option<String>,
    pub reason: String,
}

impl Failure {
    fn feed(reason: String) -> Failure {
        Failure {
            version: None,
            reason,
        }
    }
}

/// Looks for a release newer than the installed one and readies it: download,
/// verify and stage in `staging`. A release named `skip` is passed over, as
/// one that failed before. Blocks: run it off the UI thread.
pub fn check(
    installed: &Installed,
    skip: Option<&str>,
    staging: &Path,
    feed: &impl Feed,
    runner: &impl Runner,
) -> Result<Checked, Failure> {
    let release = feed.latest().map_err(Failure::feed)?;
    let version = release.tag_name.trim_start_matches('v');
    if !newer(version, &installed.version).map_err(Failure::feed)? || skip == Some(version) {
        return Ok(Checked::Current);
    }
    let Some(name) = asset::name(version, installed.platform) else {
        return Ok(Checked::Current);
    };
    // A release published before CI attaches its build isn't an update yet.
    let Some(asset) = release.assets.iter().find(|asset| asset.name == name) else {
        return Ok(Checked::Current);
    };
    let failed = |reason: String| Failure {
        version: Some(version.to_owned()),
        reason,
    };
    let path = stage(installed, version, asset, staging, feed, runner).map_err(failed)?;
    Ok(Checked::Ready(Staged {
        version: version.to_owned(),
        summary: release.body.as_deref().map(summary).unwrap_or_default(),
        page: release.html_url.clone(),
        path,
        installed: installed.clone(),
    }))
}

/// Downloads `asset`, checks it and puts the new app where the swap takes
/// it from.
fn stage(
    installed: &Installed,
    version: &str,
    asset: &Asset,
    staging: &Path,
    feed: &impl Feed,
    runner: &impl Runner,
) -> Result<PathBuf, String> {
    let name = &asset.name;
    let digest = (asset.digest.as_deref()).ok_or(format!("{name} has no published digest."))?;
    let _ = fs::remove_dir_all(staging);
    fs::create_dir_all(staging)
        .map_err(|error| format!("Couldn't make {}: {error}", staging.display()))?;
    let download = staging.join(name);
    feed.download(&asset.browser_download_url, &download)
        .map_err(|error| format!("Couldn't download {name}: {error}"))?;
    let bytes = fs::read(&download).map_err(|error| format!("Couldn't read {name}: {error}"))?;
    if !matches_digest(&bytes, digest) {
        return Err(format!("{name} doesn't match its published digest."));
    }
    let os = installed.platform.os;
    let unpack = |args: &[&OsStr]| {
        runner
            .run(&tool(os, if os == Os::Mac { "ditto" } else { "tar" }), args)
            .map_err(|error| format!("Couldn't unpack {name}: {error}"))
    };
    match os {
        Os::Mac => {
            unpack(&[
                OsStr::new("-x"),
                OsStr::new("-k"),
                download.as_os_str(),
                staging.as_os_str(),
            ])?;
            let app = staging.join(asset::MAC_APP);
            verify_signature(&app, &installed.path, runner)?;
            Ok(app)
        }
        // Unsigned for now: the digest above is the check.
        Os::Windows => {
            unpack(&[
                OsStr::new("-x"),
                OsStr::new("-f"),
                download.as_os_str(),
                OsStr::new("-C"),
                staging.as_os_str(),
            ])?;
            put_beside(
                &installed.path,
                &staging.join(asset::windows_exe(version, installed.platform)),
            )
        }
        // Unsigned for now: the digest above is the check.
        Os::Linux => put_beside(&installed.path, &download),
    }
}

/// Copies the new app next to the installed one, so the swap on exit is two
/// renames in one folder.
fn put_beside(installed: &Path, new: &Path) -> Result<PathBuf, String> {
    let beside = with_suffix(installed, ".new");
    fs::copy(new, &beside).map_err(|error| format!("Couldn't stage the update: {error}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&beside, fs::Permissions::from_mode(0o755))
            .map_err(|error| format!("Couldn't stage the update: {error}"))?;
    }
    Ok(beside)
}

/// The new bundle must pass Gatekeeper's own check and be signed by the team
/// that signed the running one.
fn verify_signature(app: &Path, installed: &Path, runner: &impl Runner) -> Result<(), String> {
    let codesign = tool(Os::Mac, "codesign");
    let strict = ["--verify", "--deep", "--strict"].map(OsStr::new);
    runner
        .run(&codesign, &[&strict[..], &[app.as_os_str()]].concat())
        .map_err(|error| format!("The new MUDling.app isn't signed properly: {error}"))?;
    let team = |bundle: &Path| {
        let details = [
            OsStr::new("-d"),
            OsStr::new("--verbose=2"),
            bundle.as_os_str(),
        ];
        let ran = runner.run(&codesign, &details).map_err(|error| {
            format!(
                "Couldn't read the signature of {}: {error}",
                bundle.display()
            )
        })?;
        // codesign writes the details to standard error.
        Ok::<_, String>(parse_team(&ran.stderr))
    };
    let (running, new) = (team(installed)?, team(app)?);
    if running != new {
        let running = running.as_deref().unwrap_or("none");
        return Err(format!(
            "The new MUDling.app isn't signed by team {running}."
        ));
    }
    Ok(())
}

fn parse_team(details: &str) -> Option<String> {
    details
        .lines()
        .find_map(|line| line.strip_prefix("TeamIdentifier="))
        .filter(|team| *team != "not set")
        .map(str::to_owned)
}

fn newer(candidate: &str, current: &str) -> Result<bool, String> {
    Ok(parse_version(candidate)? > parse_version(current)?)
}

fn parse_version(version: &str) -> Result<Vec<u64>, String> {
    version
        .split('.')
        .map(|part| {
            part.parse()
                .map_err(|_| format!("The version {version} can't be read."))
        })
        .collect()
}

fn matches_digest(bytes: &[u8], digest: &str) -> bool {
    let actual: String = Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    digest.strip_prefix("sha256:") == Some(actual.as_str())
}

/// The `## Summary` section's lines, up to the next heading, such as the
/// generated `## What's Changed`, without their bullets.
fn summary(notes: &str) -> Vec<String> {
    notes
        .lines()
        .map(str::trim)
        .skip_while(|line| *line != "## Summary")
        .skip(1)
        .take_while(|line| !line.starts_with('#'))
        .map(|line| line.trim_start_matches(['*', '-']).trim())
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = OsString::from(path.as_os_str());
    name.push(suffix);
    PathBuf::from(name)
}

/// Waits for the app to exit, swaps the bundles, and puts the old one back if
/// the new one won't move in. A failure is written down for the next launch
/// to say. Then it opens MUDling again, if asked, with the arguments after
/// the first six.
const MAC_SWAP: &str = r#"
pid=$1 installed=$2 new=$3 version=$4 failed=$5 relaunch=$6
shift 6
while kill -0 "$pid" 2>/dev/null; do sleep 0.2; done
previous="$installed.previous"
rm -rf "$previous"
if mv "$installed" "$previous"; then
    if mv "$new" "$installed"; then
        rm -rf "$previous"
    else
        mv "$previous" "$installed"
        printf '%s\nThe new MUDling.app could not be moved into place.\n' "$version" > "$failed"
    fi
else
    printf '%s\nMUDling.app could not be moved aside.\n' "$version" > "$failed"
fi
if [ "$relaunch" = 1 ]; then open "$installed" --args "$@"; fi
"#;

impl Staged {
    /// A made-up release with nothing on disk, for measured runs, checks and
    /// images. It's never put in.
    pub fn sample(version: &str, summary: &[&str]) -> Staged {
        Staged {
            version: version.to_owned(),
            summary: summary.iter().map(|line| (*line).to_owned()).collect(),
            page: "https://github.com/Vyttle-LLC/mudling/releases".to_owned(),
            path: PathBuf::new(),
            installed: Installed {
                platform: Platform {
                    os: Os::Mac,
                    arch: "aarch64",
                },
                path: PathBuf::new(),
                version: "0.8.2".to_owned(),
            },
        }
    }

    pub fn installed(&self) -> &Installed {
        &self.installed
    }

    /// Puts the update in once this process has exited, then opens MUDling
    /// again with `relaunch`'s arguments, if any. Call it once everything is
    /// saved. `cache` is where a failed swap writes down why.
    pub fn install_on_exit(&self, relaunch: Option<&[String]>, cache: &Path) -> Result<(), String> {
        let failed = cache.join(FAILED_SWAP);
        let installed = &self.installed.path;
        match self.installed.platform.os {
            Os::Mac => {
                let mut swap = Command::new("/bin/sh");
                swap.args(["-c", MAC_SWAP, "swap"])
                    .arg(std::process::id().to_string())
                    .arg(installed)
                    .arg(&self.path)
                    .arg(&self.version)
                    .arg(&failed)
                    .arg(if relaunch.is_some() { "1" } else { "0" })
                    .args(relaunch.unwrap_or_default())
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null());
                #[cfg(unix)]
                {
                    // Its own group, so nothing that stops the app stops the swap.
                    use std::os::unix::process::CommandExt;
                    swap.process_group(0);
                }
                swap.spawn()
                    .map(drop)
                    .map_err(|error| format!("Couldn't start the update: {error}"))
            }
            // A running program can't be overwritten on Windows, but it can be
            // renamed, so the old one steps aside for the new.
            Os::Windows | Os::Linux => {
                if let Err(reason) = swap_file(installed, &self.path) {
                    let _ = fs::write(&failed, format!("{}\n{reason}\n", self.version));
                }
                if let Some(arguments) = relaunch {
                    Command::new(installed)
                        .args(arguments)
                        .stdin(Stdio::null())
                        .spawn()
                        .map_err(|error| format!("Couldn't open MUDling again: {error}"))?;
                }
                Ok(())
            }
        }
    }
}

/// Moves `installed` aside to `<name>.old` and `new` into its place, or puts
/// it back. The next launch removes the old copy.
fn swap_file(installed: &Path, new: &Path) -> Result<(), String> {
    let old = with_suffix(installed, ".old");
    let _ = fs::remove_file(&old);
    fs::rename(installed, &old)
        .map_err(|error| format!("{} couldn't be moved aside: {error}", installed.display()))?;
    if let Err(error) = fs::rename(new, installed) {
        let _ = fs::rename(&old, installed);
        return Err(format!(
            "The new copy couldn't be moved into place: {error}"
        ));
    }
    Ok(())
}

/// What the last exit's swap left: the old copy it moved aside, which goes
/// now, and if the swap failed, the version and why, to say once.
pub fn after_last_exit(installed: &Installed, cache: &Path) -> Option<Failure> {
    let _ = fs::remove_file(with_suffix(&installed.path, ".old"));
    let failed = cache.join(FAILED_SWAP);
    let written = fs::read_to_string(&failed).ok()?;
    let _ = fs::remove_file(&failed);
    let (version, reason) = written.split_once('\n')?;
    Some(Failure {
        version: Some(version.to_owned()),
        reason: reason.trim().to_owned(),
    })
}
