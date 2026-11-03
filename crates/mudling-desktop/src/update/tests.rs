//! A fake releases feed and fake programs stand in for GitHub, curl and
//! codesign: these tests never reach the network.

use super::state::CHECK_EVERY_MILLIS;
use super::*;
use mudling_core::persist::Timestamp;
use std::cell::RefCell;
use std::collections::HashMap;
use tempfile::TempDir;

const MAC_ARM: Platform = Platform {
    os: Os::Mac,
    arch: "aarch64",
};
const LINUX_X64: Platform = Platform {
    os: Os::Linux,
    arch: "x86_64",
};
const TEAM: &str = VYTTLE_TEAM;

/// A release feed with one release and the bytes of its downloads.
struct FakeFeed {
    release: serde_json::Value,
    files: HashMap<String, Vec<u8>>,
}

impl FakeFeed {
    /// `version`'s release, with `asset` holding `bytes` and `digest`.
    fn with(version: &str, asset: &str, bytes: &[u8], digest: Option<String>) -> FakeFeed {
        let url = format!("https://example.invalid/{asset}");
        FakeFeed {
            release: serde_json::json!({
                "tag_name": format!("v{version}"),
                "html_url": "https://example.invalid/release",
                "body": "## Summary\n* Chat channels\n\n## What's Changed\n* A fix",
                "assets": [{ "name": asset, "browser_download_url": url, "digest": digest }],
            }),
            files: HashMap::from([(url, bytes.to_vec())]),
        }
    }
}

impl Feed for FakeFeed {
    fn latest(&self) -> Result<Release, String> {
        parse_feed(&format!("{}\n200", self.release))
    }

    fn download(&self, url: &str, to: &Path) -> Result<(), String> {
        let bytes = self.files.get(url).ok_or("no such file")?;
        fs::write(to, bytes).map_err(|error| error.to_string())
    }
}

/// Programs that succeed, with codesign reporting `team` as the signer.
struct FakeRunner {
    team: &'static str,
    ran: RefCell<Vec<String>>,
}

impl FakeRunner {
    fn signed_by(team: &'static str) -> FakeRunner {
        FakeRunner {
            team,
            ran: RefCell::default(),
        }
    }
}

impl Runner for FakeRunner {
    fn run(&self, program: &Path, args: &[&OsStr]) -> Result<Ran, String> {
        let words: Vec<String> = args
            .iter()
            .map(|arg| arg.to_string_lossy().into())
            .collect();
        self.ran
            .borrow_mut()
            .push(format!("{} {}", program.display(), words.join(" ")));
        Ok(Ran {
            stdout: Vec::new(),
            stderr: format!(
                "Identifier=com.vyttle.mudling\nTeamIdentifier={}\n",
                self.team
            ),
        })
    }
}

fn digest_of(bytes: &[u8]) -> String {
    let hex: String = Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("sha256:{hex}")
}

fn installed(platform: Platform, path: PathBuf) -> Installed {
    Installed {
        platform,
        path,
        version: "0.8.2".to_owned(),
    }
}

fn failure_of(checked: Result<Checked, Failure>) -> Failure {
    match checked {
        Err(failure) => failure,
        Ok(_) => panic!("the release was taken"),
    }
}

#[test]
fn versions_compare_by_number() {
    assert!(newer("0.1.10", "0.1.9").unwrap());
    assert!(newer("1.0.0", "0.9.99").unwrap());
    assert!(!newer("0.8.2", "0.8.2").unwrap());
    assert!(!newer("0.8.1", "0.8.2").unwrap());
    assert!(newer("0.9.x", "0.8.2").is_err());
}

#[test]
fn each_os_and_architecture_updates_from_its_own_asset() {
    let name = |os, arch| asset::name("0.9.0", Platform { os, arch });
    let names = [
        (Os::Mac, "aarch64", "MUDling-0.9.0-macos-arm64.zip"),
        (Os::Mac, "x86_64", "MUDling-0.9.0-macos-x86_64.zip"),
        (Os::Windows, "x86_64", "MUDling-0.9.0-windows-x64.zip"),
        (Os::Windows, "aarch64", "MUDling-0.9.0-windows-arm64.zip"),
        (Os::Linux, "x86_64", "MUDling-0.9.0-linux-x64.AppImage"),
        (Os::Linux, "aarch64", "MUDling-0.9.0-linux-arm64.AppImage"),
    ];
    for (os, arch, expected) in names {
        assert_eq!(name(os, arch).as_deref(), Some(expected), "{os:?} {arch}");
    }
    assert_eq!(name(Os::Linux, "riscv64"), None);
    let windows_arm = Platform {
        os: Os::Windows,
        arch: "aarch64",
    };
    assert_eq!(
        asset::windows_exe("0.9.0", windows_arm),
        Path::new("MUDling-0.9.0-windows-arm64").join("MUDling.exe")
    );
}

#[test]
fn a_release_without_this_computers_build_is_not_an_update() {
    let folder = TempDir::new().unwrap();
    let feed = FakeFeed::with("0.9.0", "MUDling-0.9.0-macos-x86_64.zip", b"app", None);
    let checked = check(
        &installed(MAC_ARM, folder.path().join("MUDling.app")),
        None,
        &folder.path().join("update"),
        &feed,
        &FakeRunner::signed_by(TEAM),
    );
    assert!(matches!(checked, Ok(Checked::Current)));
}

#[test]
fn a_download_that_doesnt_match_its_digest_is_refused() {
    let folder = TempDir::new().unwrap();
    let asset = "MUDling-0.9.0-linux-x64.AppImage";
    let feed = FakeFeed::with("0.9.0", asset, b"tampered", Some(digest_of(b"built")));
    let appimage = folder.path().join("MUDling.AppImage");
    fs::write(&appimage, "old").unwrap();
    let staging = folder.path().join("update");
    let checked = check(
        &installed(LINUX_X64, appimage.clone()),
        None,
        &staging,
        &feed,
        &FakeRunner::signed_by(TEAM),
    );
    let failure = failure_of(checked);
    assert_eq!(failure.version.as_deref(), Some("0.9.0"));
    assert!(
        failure
            .reason
            .contains("doesn't match its published digest")
    );
    assert_eq!(fs::read_to_string(&appimage).unwrap(), "old");
    assert!(
        !with_suffix(&appimage, ".new").exists(),
        "nothing is staged"
    );
}

#[test]
fn an_asset_with_no_digest_is_refused_before_it_downloads() {
    let folder = TempDir::new().unwrap();
    let feed = FakeFeed::with("0.9.0", "MUDling-0.9.0-linux-x64.AppImage", b"built", None);
    let failure = failure_of(check(
        &installed(LINUX_X64, folder.path().join("MUDling.AppImage")),
        None,
        &folder.path().join("update"),
        &feed,
        &FakeRunner::signed_by(TEAM),
    ));
    assert!(failure.reason.contains("has no published digest"));
    assert!(!folder.path().join("update").exists());
}

#[test]
fn a_bundle_signed_by_another_team_is_refused() {
    let folder = TempDir::new().unwrap();
    let asset = "MUDling-0.9.0-macos-arm64.zip";
    let feed = FakeFeed::with("0.9.0", asset, b"zip", Some(digest_of(b"zip")));
    let check_signed_by = |team| {
        check(
            &installed(MAC_ARM, folder.path().join("Applications/MUDling.app")),
            None,
            &folder.path().join("update"),
            &feed,
            &FakeRunner::signed_by(team),
        )
    };
    let failure = failure_of(check_signed_by("ZZZZZ99999"));
    assert!(
        failure
            .reason
            .contains("isn't signed by Vyttle's team, 34674BKS72")
    );

    let runner = FakeRunner::signed_by(TEAM);
    let ready = check(
        &installed(MAC_ARM, folder.path().join("Applications/MUDling.app")),
        None,
        &folder.path().join("update"),
        &feed,
        &runner,
    );
    let Ok(Checked::Ready(staged)) = ready else {
        panic!("Vyttle's bundle is taken");
    };
    assert_eq!(staged.version, "0.9.0");
    assert_eq!(staged.summary, ["Chat channels"]);
    assert_eq!(staged.path, folder.path().join("update/MUDling.app"));
    let ran = runner.ran.borrow();
    assert!(
        ran.iter()
            .any(|run| run.contains("codesign --verify --deep --strict"))
    );
}

#[test]
fn a_verified_appimage_waits_beside_the_installed_one() {
    let folder = TempDir::new().unwrap();
    let asset = "MUDling-0.9.0-linux-x64.AppImage";
    let feed = FakeFeed::with("0.9.0", asset, b"new", Some(digest_of(b"new")));
    let appimage = folder.path().join("MUDling.AppImage");
    fs::write(&appimage, "old").unwrap();
    let checked = check(
        &installed(LINUX_X64, appimage.clone()),
        None,
        &folder.path().join("update"),
        &feed,
        &FakeRunner::signed_by(TEAM),
    );
    let Ok(Checked::Ready(staged)) = checked else {
        panic!("the release is ready");
    };
    assert_eq!(fs::read_to_string(&staged.path).unwrap(), "new");
    assert_eq!(
        fs::read_to_string(&appimage).unwrap(),
        "old",
        "it waits for exit"
    );

    swap_file(&appimage, &staged.path).unwrap();
    assert_eq!(fs::read_to_string(&appimage).unwrap(), "new");
    assert_eq!(after_last_exit(&folder.path().join("cache")), None);
    remove_old_copies(&appimage);
    assert!(
        !with_suffix(&appimage, ".old").exists(),
        "the next launch tidies up"
    );
}

#[test]
fn a_stale_old_copy_never_stops_the_next_update() {
    let folder = TempDir::new().unwrap();
    let exe = folder.path().join("MUDling.exe");
    let new = folder.path().join("MUDling.exe.new");
    let old = with_suffix(&exe, ".old");
    // One that can't be deleted, as Windows holds a copy still running: a
    // folder with a file in it refuses `remove_file` on every OS.
    fs::create_dir(&old).unwrap();
    fs::write(old.join("held"), "").unwrap();
    fs::write(&exe, "0.8.2").unwrap();
    fs::write(&new, "0.9.0").unwrap();

    swap_file(&exe, &new).unwrap();
    assert_eq!(fs::read_to_string(&exe).unwrap(), "0.9.0");
    assert_eq!(fs::read_to_string(&old).unwrap(), "0.8.2");
    let aside = fs::read_dir(folder.path())
        .unwrap()
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("MUDling.exe.old-")
        })
        .count();
    assert_eq!(aside, 1, "the held copy was renamed out of the way");

    // And the update after that, over a plain stale `.old`.
    fs::write(&new, "0.9.1").unwrap();
    swap_file(&exe, &new).unwrap();
    assert_eq!(fs::read_to_string(&exe).unwrap(), "0.9.1");
}

#[test]
fn old_copies_go_once_nothing_holds_them() {
    let folder = TempDir::new().unwrap();
    let exe = folder.path().join("MUDling.exe");
    for name in [
        "MUDling.exe",
        "MUDling.exe.old",
        "MUDling.exe.old-1793680000",
    ] {
        fs::write(folder.path().join(name), "").unwrap();
    }
    remove_old_copies(&exe);
    let left: Vec<_> = fs::read_dir(folder.path())
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name())
        .collect();
    assert_eq!(left, ["MUDling.exe"]);
}

#[test]
fn a_swap_that_fails_leaves_the_installed_copy_in_place() {
    let folder = TempDir::new().unwrap();
    let exe = folder.path().join("MUDling.exe");
    fs::write(&exe, "old").unwrap();
    let missing = folder.path().join("MUDling.exe.new");
    assert!(swap_file(&exe, &missing).is_err());
    assert_eq!(fs::read_to_string(&exe).unwrap(), "old");
}

#[test]
fn a_failed_swap_is_said_once_on_the_next_launch() {
    let folder = TempDir::new().unwrap();
    fs::write(
        folder.path().join(FAILED_SWAP),
        "0.9.0\nThe new copy couldn't be moved into place.\n",
    )
    .unwrap();
    let failure = after_last_exit(folder.path()).unwrap();
    assert_eq!(failure.version.as_deref(), Some("0.9.0"));
    assert_eq!(after_last_exit(folder.path()), None);
}

#[cfg(unix)]
#[test]
fn the_mac_bundle_swaps_after_the_app_exits() {
    let root = TempDir::new().unwrap();
    let installed = root.path().join("MUDling.app");
    let app = root.path().join("update/MUDling.app");
    fs::create_dir_all(&installed).unwrap();
    fs::create_dir_all(&app).unwrap();
    fs::write(installed.join("version"), "old").unwrap();
    fs::write(app.join("version"), "new").unwrap();
    let exited = Command::new("/usr/bin/true").spawn().unwrap();
    let pid = exited.id();
    exited.wait_with_output().unwrap();

    let status = Command::new("/bin/sh")
        .args(["-c", MAC_SWAP, "swap", &pid.to_string()])
        .arg(&installed)
        .arg(&app)
        .args(["0.9.0", "/nonexistent/failed", "0"])
        .status()
        .unwrap();

    assert!(status.success());
    assert_eq!(
        fs::read_to_string(installed.join("version")).unwrap(),
        "new"
    );
    assert!(!app.exists());
    assert!(!root.path().join("MUDling.app.previous").exists());
}

#[test]
fn a_build_from_source_never_updates() {
    let always = |_: &Path| true;
    let exe = Path::new("/Applications/MUDling.app/Contents/MacOS/mudling-desktop");
    assert_eq!(
        eligibility(None, Some(MAC_ARM), exe, None, always),
        Eligibility::SourceBuild
    );
    let mut updates = Updates::new(Eligibility::SourceBuild, UpdateChoices::default(), None);
    assert_eq!(updates.begin_check(true), None);
    assert!(updates.status().cant.unwrap().contains("from source"));
}

#[test]
fn only_a_copy_this_user_can_replace_updates() {
    let exe = Path::new("/Applications/MUDling.app/Contents/MacOS/mudling-desktop");
    let release = Some("0.8.2");
    let Eligibility::Updates(found) = eligibility(release, Some(MAC_ARM), exe, None, |_| true)
    else {
        panic!("a writable bundle updates");
    };
    assert_eq!(found.path, Path::new("/Applications/MUDling.app"));
    assert_eq!(
        eligibility(release, Some(MAC_ARM), exe, None, |_| false),
        Eligibility::CantReplace(NOT_WRITABLE)
    );
    let loose = Path::new("/Users/someone/mudling-desktop");
    assert_eq!(
        eligibility(release, Some(MAC_ARM), loose, None, |_| true),
        Eligibility::CantReplace(NOT_A_BUNDLE)
    );
    let unpacked = Path::new("/opt/mudling/mudling-desktop");
    assert_eq!(
        eligibility(release, Some(LINUX_X64), unpacked, None, |_| true),
        Eligibility::CantReplace(NOT_AN_APPIMAGE)
    );
    let appimage = Path::new("/home/someone/.local/bin/MUDling.AppImage");
    assert!(matches!(
        eligibility(release, Some(LINUX_X64), unpacked, Some(appimage), |_| true),
        Eligibility::Updates(Installed { path, .. }) if path == appimage
    ));
}

fn staged(version: &str) -> Staged {
    Staged {
        version: version.to_owned(),
        summary: Vec::new(),
        page: String::new(),
        path: PathBuf::new(),
        installed: installed(LINUX_X64, PathBuf::new()),
    }
}

fn updates() -> Updates {
    let copy = installed(LINUX_X64, PathBuf::from("/apps/MUDling.AppImage"));
    Updates::new(Eligibility::Updates(copy), UpdateChoices::default(), None)
}

#[test]
fn later_hides_the_card_until_the_next_release() {
    let mut updates = updates();
    updates.begin_check(false).unwrap();
    updates.finish_check(Ok(Checked::Ready(staged("0.9.0"))), Timestamp(1));
    assert_eq!(updates.status().card().unwrap().version, "0.9.0");

    updates.later();
    let status = updates.status();
    assert_eq!(status.card(), None);
    assert!(status.ready.is_some(), "the dot stays");

    // The next launch finds the next release.
    let choices = updates.choices().clone();
    let copy = updates.installed().unwrap().clone();
    let mut next = Updates::new(Eligibility::Updates(copy), choices, None);
    next.begin_check(false).unwrap();
    next.finish_check(Ok(Checked::Ready(staged("0.9.1"))), Timestamp(2));
    assert_eq!(next.status().card().unwrap().version, "0.9.1");
}

#[test]
fn a_release_that_failed_is_passed_over_until_check_now() {
    let mut updates = updates();
    updates.begin_check(false).unwrap();
    let refused = Failure {
        version: Some("0.9.0".to_owned()),
        reason: "It doesn't match.".to_owned(),
    };
    updates.finish_check(Err(refused), Timestamp(1));
    let (title, _) = updates.status().failure.unwrap();
    assert_eq!(title, "MUDling 0.9.0 didn't go in");
    updates.dismiss_failure();
    assert_eq!(updates.status().failure, None, "said once");

    let (_, skip) = updates.begin_check(false).unwrap();
    assert_eq!(skip.as_deref(), Some("0.9.0"));
    updates.finish_check(Ok(Checked::Current), Timestamp(2));
    let (_, skip) = updates.begin_check(true).unwrap();
    assert_eq!(skip, None, "Check now tries it again");
}

#[test]
fn the_daily_check_waits_a_day_and_for_the_switch() {
    let mut updates = updates();
    assert!(updates.due(Timestamp(0)));
    updates.begin_check(false).unwrap();
    updates.finish_check(Ok(Checked::Current), Timestamp(1_000));
    assert!(!updates.due(Timestamp(1_000 + CHECK_EVERY_MILLIS - 1)));
    assert!(updates.due(Timestamp(1_000 + CHECK_EVERY_MILLIS)));
    updates.set_checking_on(false);
    assert!(!updates.due(Timestamp(1_000 + CHECK_EVERY_MILLIS)));
}

#[test]
fn the_summary_is_its_section_without_bullets() {
    let notes = "## Summary\r\n* Chat channels\r\n\r\n- Touch ID\r\n## What's Changed\r\n* x";
    assert_eq!(summary(notes), ["Chat channels", "Touch ID"]);
    assert!(summary("## What's Changed\n* x").is_empty());
}

#[test]
fn feed_failures_read_plainly() {
    assert_eq!(
        parse_feed("{\"message\":\"Not Found\"}\n404")
            .err()
            .unwrap(),
        "GitHub has no published release yet."
    );
    assert_eq!(
        parse_feed("{}\n403").err().unwrap(),
        "GitHub answered with HTTP 403."
    );
}

#[test]
fn the_signing_team_is_read_from_codesign() {
    assert_eq!(
        parse_team("Identifier=x\nTeamIdentifier=ABCDE12345\n").as_deref(),
        Some("ABCDE12345")
    );
    assert_eq!(
        parse_team("Signature=adhoc\nTeamIdentifier=not set\n"),
        None
    );
}
