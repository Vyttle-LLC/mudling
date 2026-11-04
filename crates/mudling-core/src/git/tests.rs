//! Every remote here is a bare repo in a temp folder or a socket on this
//! machine: no network, and no real credentials.

use super::*;
use git2::Repository;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, SystemTime};
use tempfile::TempDir;

const OLD: Duration = Duration::from_secs(3600);

fn empty_repo() -> (TempDir, Repo) {
    let folder = tempfile::tempdir().unwrap();
    let repo = Repo::open_or_init(folder.path()).unwrap();
    (folder, repo)
}

/// Opens a repo this process just released. A child spawned by a parallel test
/// shares the owner lock for the moment before its exec, which a loaded
/// machine can stretch, so this retries until a generous hang guard.
fn reopen(folder: &Path) -> Repo {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match Repo::open_or_init(folder) {
            Err(GitError::Locked(_)) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(10));
            }
            opened => return opened.unwrap(),
        }
    }
}

/// Leaves the repo as a process killed in the middle of a write does.
fn die_mid_write(repo: Repo) {
    std::mem::forget(repo.mark_write().unwrap());
}

fn write(folder: &Path, name: &str, contents: &str) {
    fs::write(folder.join(name), contents).unwrap();
}

fn commit(repo: &Repo, folder: &Path, name: &str, contents: &str) -> Oid {
    write(folder, name, contents);
    repo.commit_all(&format!("write {name}")).unwrap().unwrap()
}

struct Device {
    folder: PathBuf,
    repo: Repo,
}

/// A bare remote and two devices that both point at it.
struct Devices {
    _folder: TempDir,
    remote: PathBuf,
    a: Device,
    b: Device,
}

fn two_devices() -> Devices {
    let folder = tempfile::tempdir().unwrap();
    let remote = folder.path().join("remote.git");
    Repository::init_bare(&remote).unwrap();
    let device = |name: &str| {
        let path = folder.path().join(name);
        let repo = Repo::open_or_init(&path).unwrap();
        repo.repo
            .remote("origin", remote.to_str().unwrap())
            .unwrap();
        Device { folder: path, repo }
    };
    let (a, b) = (device("a"), device("b"));
    Devices {
        _folder: folder,
        remote,
        a,
        b,
    }
}

fn remote_tip(remote: &Path) -> Option<Oid> {
    Repository::open_bare(remote)
        .unwrap()
        .refname_to_id("refs/heads/main")
        .ok()
}

fn commit_count(repo: &Repo) -> usize {
    let mut walk = repo.repo.revwalk().unwrap();
    walk.push_head().unwrap();
    walk.count()
}

fn age(path: &Path, by: Duration) {
    let file = File::options().write(true).open(path).unwrap();
    file.set_modified(SystemTime::now() - by).unwrap();
}

#[test]
fn an_empty_folder_becomes_a_repo_on_main_and_an_existing_one_is_reused() {
    let (folder, repo) = empty_repo();
    assert_eq!(repo.current_branch().unwrap(), "main");
    let id = commit(&repo, folder.path(), "mudling.json", "{}");
    drop(repo);

    let reopened = reopen(folder.path());
    assert_eq!(reopened.branch_tip("main").unwrap(), Some(id));
}

#[test]
fn a_folder_with_other_files_is_not_taken_over() {
    let folder = tempfile::tempdir().unwrap();
    write(folder.path(), "taxes.txt", "mine");

    assert_eq!(
        Repo::open_or_init(folder.path()).err(),
        Some(GitError::FolderNotEmpty)
    );
}

#[test]
fn commit_all_stages_changes_and_removals_and_skips_when_nothing_changed() {
    let (folder, repo) = empty_repo();
    assert_eq!(repo.commit_all("nothing yet").unwrap(), None);

    commit(&repo, folder.path(), "keep.json", "1");
    commit(&repo, folder.path(), "drop.json", "2");
    assert_eq!(repo.commit_all("no change").unwrap(), None);

    fs::remove_file(folder.path().join("drop.json")).unwrap();
    let id = repo.commit_all("removed").unwrap().unwrap();
    let tree = repo.repo.find_commit(id).unwrap().tree().unwrap();
    assert!(tree.get_name("keep.json").is_some());
    assert!(tree.get_name("drop.json").is_none());
}

#[test]
fn push_then_fetch_carries_a_commit_between_devices() {
    let Devices {
        _folder,
        remote,
        a,
        b,
    } = two_devices();
    assert_eq!(b.repo.fetch("origin", "main").unwrap(), None);

    let id = commit(&a.repo, &a.folder, "servers.json", "[]");
    a.repo.push("origin", "main").unwrap();

    assert_eq!(remote_tip(&remote), Some(id));
    assert_eq!(b.repo.fetch("origin", "main").unwrap(), Some(id));
    assert_eq!(b.repo.branch_tip("main").unwrap(), None);
    b.repo.move_branch("main", None, id).unwrap();
    assert_eq!(b.repo.branch_tip("main").unwrap(), Some(id));
}

#[test]
fn pushing_behind_the_remote_fails_typed_and_loses_nothing() {
    let Devices {
        _folder,
        remote,
        a,
        b,
    } = two_devices();
    let first = commit(&a.repo, &a.folder, "a.json", "1");
    a.repo.push("origin", "main").unwrap();
    let local = commit(&b.repo, &b.folder, "b.json", "2");

    let failure = b.repo.push("origin", "main").unwrap_err();

    assert!(
        matches!(failure, GitError::NonFastForward(_)),
        "{failure:?}"
    );
    assert_eq!(remote_tip(&remote), Some(first));
    assert_eq!(b.repo.branch_tip("main").unwrap(), Some(local));
}

#[test]
fn a_server_rejecting_a_push_is_told_apart_from_being_ahead() {
    let behind = push_rejected("failed to update ref: fetch first".to_owned());
    assert!(matches!(behind, GitError::NonFastForward(_)));

    let hook = push_rejected("pre-receive hook declined".to_owned());
    assert!(matches!(hook, GitError::RemoteRefused(_)));
}

#[test]
fn a_push_that_loses_a_race_on_the_server_is_told_as_the_remote_being_ahead() {
    let lost = [
        "cannot lock ref 'refs/heads/main': is at 1 but expected 2",
        "failed to update ref",
        "failed to lock file 'remote.git/refs/heads/main.lock' for writing",
        "fetch first",
    ];
    for reason in lost {
        let error = push_rejected(reason.to_owned());
        assert!(matches!(error, GitError::NonFastForward(_)), "{reason}");
    }
    let refused = push_rejected("pre-receive hook declined".to_owned());
    assert!(matches!(refused, GitError::RemoteRefused(_)));
}

#[test]
fn a_session_starts_after_the_last_finished_commit_or_what_the_remote_holds() {
    let devices = two_devices();
    let Device { folder, repo } = &devices.a;
    let pushed = commit(repo, folder, "a", "1");
    repo.push("origin", "main").unwrap();
    let session = |name: &str| {
        write(folder, name, "1");
        repo.commit_all("Checkpoint").unwrap().unwrap()
    };
    session("b");
    assert_eq!(
        repo.session_start("origin", "main", "Checkpoint").unwrap(),
        Some(pushed)
    );

    let finished = commit(repo, folder, "c", "1");
    session("d");
    session("e");
    assert_eq!(
        repo.session_start("origin", "main", "Checkpoint").unwrap(),
        Some(finished)
    );
}

#[test]
fn squash_folds_a_session_into_one_commit_with_the_same_files() {
    let (folder, repo) = empty_repo();
    let base = commit(&repo, folder.path(), "a.json", "1");
    commit(&repo, folder.path(), "b.json", "2");
    let tip = commit(&repo, folder.path(), "a.json", "3");

    let squashed = repo.squash("main", base, "Game · Hero · 2h10").unwrap();

    let commit = repo.repo.find_commit(squashed).unwrap();
    assert_eq!(commit.parent_ids().collect::<Vec<_>>(), [base]);
    assert_eq!(commit.message().unwrap(), "Game · Hero · 2h10");
    assert_eq!(
        commit.tree_id(),
        repo.repo.find_commit(tip).unwrap().tree_id()
    );
    assert_eq!(repo.branch_tip("main").unwrap(), Some(squashed));
    assert_eq!(repo.commit_all("no change").unwrap(), None);
}

#[test]
fn squash_with_nothing_after_the_base_changes_nothing() {
    let (folder, repo) = empty_repo();
    let base = commit(&repo, folder.path(), "a.json", "1");

    assert_eq!(repo.squash("main", base, "empty").unwrap(), base);
    assert_eq!(commit_count(&repo), 1);
}

#[test]
fn squash_refuses_a_base_the_branch_does_not_build_on() {
    let (folder, repo) = empty_repo();
    let first = commit(&repo, folder.path(), "a.json", "1");
    let second = commit(&repo, folder.path(), "a.json", "2");
    repo.move_branch("main", Some(second), first).unwrap();

    assert!(matches!(
        repo.squash("main", second, "no"),
        Err(GitError::Failed(_))
    ));
    assert_eq!(repo.branch_tip("main").unwrap(), Some(first));
}

#[test]
fn a_crash_between_writing_the_squash_and_moving_the_branch_leaves_the_repo_valid() {
    let (folder, repo) = empty_repo();
    let base = commit(&repo, folder.path(), "a.json", "1");
    let tip = commit(&repo, folder.path(), "b.json", "2");

    // Crash: the commit is written, and the process dies before the move.
    let orphan = repo.write_squash(tip, base, "squash").unwrap();
    drop(repo);

    let repo = reopen(folder.path());
    assert_eq!(repo.branch_tip("main").unwrap(), Some(tip));
    assert_eq!(commit_count(&repo), 2);
    assert!(repo.repo.statuses(None).unwrap().is_empty());
    assert!(repo.repo.find_commit(orphan).is_ok());

    let squashed = repo.squash("main", base, "squash").unwrap();
    assert_eq!(repo.branch_tip("main").unwrap(), Some(squashed));
    assert_eq!(commit_count(&repo), 2);
}

#[test]
fn move_branch_moves_nothing_if_the_branch_is_not_where_the_caller_thinks() {
    let (folder, repo) = empty_repo();
    let first = commit(&repo, folder.path(), "a.json", "1");
    let second = commit(&repo, folder.path(), "a.json", "2");

    assert!(repo.move_branch("main", Some(first), first).is_err());
    assert_eq!(repo.branch_tip("main").unwrap(), Some(second));
}

#[test]
fn locks_left_by_an_app_killed_mid_write_are_cleared_at_open_however_young() {
    let (folder, repo) = empty_repo();
    let git_dir = repo.repo.path().to_owned();
    commit(&repo, folder.path(), "a.json", "1");
    die_mid_write(repo);
    let locks = [
        git_dir.join("index.lock"),
        git_dir.join("refs/heads/main.lock"),
    ];
    for lock in &locks {
        fs::write(lock, "").unwrap();
    }

    let repo = reopen(folder.path());

    assert!(locks.iter().all(|lock| !lock.exists()));
    commit(&repo, folder.path(), "a.json", "2");
}

#[test]
fn a_lock_may_be_live_while_another_mudling_has_the_repo_open() {
    let (folder, holder) = empty_repo();
    let _writing = holder.mark_write().unwrap();
    let lock = holder.repo.path().join("index.lock");
    fs::write(&lock, "").unwrap();

    let refused = Repo::open_or_init(folder.path());

    assert!(matches!(refused.err(), Some(GitError::Locked(_))));
    assert!(lock.exists());
}

#[test]
fn a_lock_older_than_the_killed_write_belongs_to_another_git_and_stays() {
    let (folder, repo) = empty_repo();
    let lock = repo.repo.path().join("index.lock");
    fs::write(&lock, "").unwrap();
    age(&lock, OLD);
    die_mid_write(repo);

    reopen(folder.path());

    assert!(lock.exists());
}

#[test]
fn another_gits_lock_stays_however_old_and_blocks_as_locked_until_released() {
    let (folder, repo) = empty_repo();
    commit(&repo, folder.path(), "a.json", "1");
    let lock = repo.repo.path().join("index.lock");
    fs::write(&lock, "").unwrap();
    age(&lock, OLD);
    drop(repo);

    let repo = reopen(folder.path());
    assert!(lock.exists());
    write(folder.path(), "a.json", "2");
    let failure = repo.commit_all("blocked").unwrap_err();
    assert!(matches!(failure, GitError::Locked(_)), "{failure:?}");
    assert!(lock.exists());

    fs::remove_file(&lock).unwrap();
    assert!(repo.commit_all("retried").unwrap().is_some());
}

#[test]
fn the_write_marker_goes_when_a_write_ends_or_fails() {
    let (folder, repo) = empty_repo();
    let marker = repo.repo.path().join("mudling-writing");
    commit(&repo, folder.path(), "a.json", "1");
    assert!(!marker.exists());

    fs::write(repo.repo.path().join("index.lock"), "").unwrap();
    write(folder.path(), "a.json", "2");
    assert!(repo.commit_all("blocked").is_err());
    assert!(!marker.exists());
}

#[test]
fn a_locked_branch_fails_as_locked() {
    let (folder, repo) = empty_repo();
    let first = commit(&repo, folder.path(), "a.json", "1");
    let second = commit(&repo, folder.path(), "a.json", "2");
    fs::write(repo.repo.path().join("refs/heads/main.lock"), "").unwrap();

    let failure = repo.move_branch("main", Some(second), first).unwrap_err();

    assert!(matches!(failure, GitError::Locked(_)), "{failure:?}");
}

/// A sync quit during a fetch gives up on it, and its git thread still holds
/// the tracking branch's lock when the quit's own fetch wants it.
#[test]
fn a_network_step_never_runs_beside_one_given_up_on() {
    let (_folder, repo) = empty_repo();
    let (entered, has_entered) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    let stop = Arc::new(AtomicBool::new(false));
    let stopper = thread::spawn({
        let stop = Arc::clone(&stop);
        move || {
            has_entered.recv().unwrap();
            stop.store(true, Ordering::Relaxed);
        }
    });

    let given_up = repo.off_thread(&Limit::stopped_by(NETWORK_LIMIT, stop), move |_| {
        entered.send(()).unwrap();
        let _ = released.recv();
        Ok(())
    });
    stopper.join().unwrap();
    let (ran, did_run) = mpsc::channel();
    let beside = repo.off_thread(&Limit::after(Duration::ZERO), move |_| {
        ran.send(()).unwrap();
        Ok(())
    });
    drop(release);

    assert!(
        matches!(given_up, Err(GitError::TimedOut(_))),
        "{given_up:?}"
    );
    assert!(matches!(beside, Err(GitError::TimedOut(_))), "{beside:?}");
    assert!(
        did_run.recv().is_err(),
        "a step ran beside the one given up on"
    );
    let after = repo.off_thread(&Limit::after(NETWORK_LIMIT), |_| Ok(()));
    assert!(after.is_ok(), "{after:?}");
}

/// A port on this machine that nothing listens on.
fn dead_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[test]
fn an_unreachable_remote_is_no_network_over_ssh_and_http() {
    let port = dead_port();
    let (_folder, repo) = empty_repo();
    repo.repo
        .remote("ssh", &format!("ssh://git@127.0.0.1:{port}/r.git"))
        .unwrap();
    repo.repo
        .remote("http", &format!("http://127.0.0.1:{port}/r.git"))
        .unwrap();

    for remote in ["ssh", "http"] {
        let failure = repo.fetch(remote, "main").unwrap_err();
        assert!(
            matches!(failure, GitError::NoNetwork(_)),
            "{remote}: {failure:?}"
        );
    }
}

/// An HTTP server that demands a login from every request and reports the
/// `Authorization` header each one carried.
fn refusing_server() -> (u16, mpsc::Receiver<Option<String>>) {
    answering_server("401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"t\"\r\nContent-Length: 0")
}

/// An HTTP server that gives every request the same status line and headers.
fn answering_server(answer: &'static str) -> (u16, mpsc::Receiver<Option<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        for client in listener.incoming() {
            let mut client = client.unwrap();
            let mut request = [0; 4096];
            let read = client.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..read]).into_owned();
            let authorization = request
                .lines()
                .find_map(|line| line.strip_prefix("Authorization: "))
                .map(str::to_owned);
            if sender.send(authorization).is_err() {
                return;
            }
            let reply = format!("HTTP/1.1 {answer}\r\nConnection: close\r\n\r\n");
            let _ = client.write_all(reply.as_bytes());
        }
    });
    (port, receiver)
}

#[test]
fn a_reachable_server_saying_no_is_a_refusal_not_a_lost_network() {
    let (port, _requests) = answering_server("404 Not Found\r\nContent-Length: 0");
    let (_folder, repo) = empty_repo();
    repo.repo
        .remote("origin", &format!("http://127.0.0.1:{port}/r.git"))
        .unwrap();

    let failure = repo.fetch("origin", "main").unwrap_err();

    assert!(matches!(failure, GitError::RemoteRefused(_)), "{failure:?}");
}

#[test]
fn a_server_in_passing_trouble_is_not_a_refusal() {
    for answer in [
        "500 Internal Server Error\r\nContent-Length: 0",
        "503 Service Unavailable\r\nContent-Length: 0",
        "429 Too Many Requests\r\nContent-Length: 0",
    ] {
        let (port, _requests) = answering_server(answer);
        let (_folder, repo) = empty_repo();
        repo.repo
            .remote("origin", &format!("http://127.0.0.1:{port}/r.git"))
            .unwrap();

        let failure = repo.fetch("origin", "main").unwrap_err();

        assert!(
            matches!(failure, GitError::Unavailable(_)),
            "{answer}: {failure:?}"
        );
    }
}

#[test]
fn a_forbidden_answer_is_auth_refused() {
    let (port, _requests) = answering_server("403 Forbidden\r\nContent-Length: 0");
    let (_folder, repo) = empty_repo();
    repo.repo
        .remote("origin", &format!("http://127.0.0.1:{port}/r.git"))
        .unwrap();

    let failure = repo.fetch("origin", "main").unwrap_err();

    assert!(matches!(failure, GitError::AuthRefused(_)), "{failure:?}");
}

/// Clears any helpers from the real gitconfigs, as a local empty value does.
fn with_helper(repo: &Repo, helper: &str) {
    let mut config = repo.repo.config().unwrap();
    config.set_multivar("credential.helper", "^$", "").unwrap();
    if !helper.is_empty() {
        config
            .set_multivar("credential.helper", "^$^", helper)
            .unwrap();
    }
}

#[test]
fn a_remote_refusing_a_machine_with_no_helpers_is_auth_refused() {
    let (port, _requests) = refusing_server();
    let (_folder, repo) = empty_repo();
    with_helper(&repo, "");
    repo.repo
        .remote("origin", &format!("http://127.0.0.1:{port}/r.git"))
        .unwrap();

    let failure = repo.fetch("origin", "main").unwrap_err();

    assert!(matches!(failure, GitError::AuthRefused(_)), "{failure:?}");
}

#[cfg(unix)]
#[test]
fn a_gitconfig_helper_signs_in_and_a_refusal_ends_as_auth_refused() {
    let (port, requests) = refusing_server();
    let (_folder, repo) = empty_repo();
    let scripts = tempfile::tempdir().unwrap();
    let helper = scripts.path().join("helper.sh");
    fs::write(
        &helper,
        "cat >/dev/null\necho username=alice\necho password=s3cret\n",
    )
    .unwrap();
    with_helper(&repo, &format!("!sh '{}'", helper.display()));
    repo.repo
        .remote("origin", &format!("http://127.0.0.1:{port}/r.git"))
        .unwrap();

    let failure = repo.fetch("origin", "main").unwrap_err();

    assert!(matches!(failure, GitError::AuthRefused(_)), "{failure:?}");
    let sent: Vec<_> = requests.try_iter().flatten().collect();
    assert_eq!(sent, ["Basic YWxpY2U6czNjcmV0"]);
}

#[test]
fn signature_falls_back_when_the_machine_has_no_git_identity() {
    let (_folder, repo) = empty_repo();
    let mut config = repo.repo.config().unwrap();
    config.set_str("user.name", "").unwrap();
    config.set_str("user.email", "").unwrap();

    let signature = repo.signature().unwrap();

    assert_eq!(signature.name().unwrap(), FALLBACK_NAME);
}

#[test]
fn history_lists_sessions_without_merges_or_checkpoints() {
    let (folder, repo) = empty_repo();
    assert_eq!(session_history(folder.path(), 5).unwrap(), []);

    commit(&repo, folder.path(), "a", "1");
    write(folder.path(), "b", "2");
    repo.commit_all("A MUD · Fikus: 3 rooms").unwrap();
    write(folder.path(), "c", "3");
    repo.commit_all(merge::CHECKPOINT).unwrap();

    let sessions = session_history(folder.path(), 1).unwrap();
    let messages: Vec<_> = sessions.iter().map(|s| s.message.as_str()).collect();
    assert_eq!(messages, ["A MUD · Fikus: 3 rooms"]);
    assert!(sessions[0].time > 0);
}

#[test]
fn signing_in_is_told_from_the_remote_and_never_quotes_it() {
    let (folder, _repo) = empty_repo();
    let git = Repository::open(folder.path()).unwrap();
    assert_eq!(sign_in_for(folder.path(), "origin"), Ok(SignIn::NoRemote));

    git.remote("origin", "git@example.org:me/data.git").unwrap();
    assert_eq!(sign_in_for(folder.path(), "origin"), Ok(SignIn::SshAgent));

    git.remote_set_url("origin", "/some/folder").unwrap();
    assert_eq!(sign_in_for(folder.path(), "origin"), Ok(SignIn::NotNeeded));
}
