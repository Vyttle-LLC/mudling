//! Signs in with what the machine already has (PRD §10): ssh-agent for SSH
//! remotes, gitconfig credential helpers for HTTPS. MUDling stores nothing.
//!
//! This runs helpers itself because `git2::Cred::credential_helper` shells out
//! to `git credential-<name>`, and no code path may need a system git.

use super::Limit;
use git2::{Config, Cred, CredentialType, ErrorClass, ErrorCode, RemoteCallbacks};
use std::env;
use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::Duration;
use url::Url;

/// How often a running helper is checked on.
const HELPER_POLL: Duration = Duration::from_millis(20);

/// libgit2 asks again after every refusal, so each way in is offered once.
/// `limit` ends any helper still running when it's up.
pub(super) fn machine_callbacks<'a>(config: Config, limit: Limit) -> RemoteCallbacks<'a> {
    let mut agent_offered = false;
    let mut helpers: Option<std::vec::IntoIter<String>> = None;
    let mut callbacks = RemoteCallbacks::new();
    callbacks.credentials(move |url, username, allowed| {
        if allowed.contains(CredentialType::USERNAME) {
            return Cred::username(username.unwrap_or("git"));
        }
        if allowed.contains(CredentialType::SSH_KEY) && !agent_offered {
            agent_offered = true;
            return Cred::ssh_key_from_agent(username.unwrap_or("git"));
        }
        if allowed.contains(CredentialType::USER_PASS_PLAINTEXT) {
            let remaining = helpers.get_or_insert_with(|| helpers_for(&config, url).into_iter());
            if let Some((user, password)) = remaining.find_map(|h| ask(&h, url, username, &limit)) {
                return Cred::userpass_plaintext(&user, &password);
            }
        }
        Err(git2::Error::new(
            ErrorCode::Auth,
            ErrorClass::Callback,
            "nothing on this machine could sign in",
        ))
    });
    callbacks
}

/// What to call a helper. Any other helper stays unnamed, because its value
/// is a command, which can hold a token.
pub(super) fn plain_name(helper: &str) -> Option<&'static str> {
    [
        ("gh", "the GitHub CLI"),
        ("manager", "Git Credential Manager"),
        ("osxkeychain", "the macOS keychain"),
        ("libsecret", "the system keyring"),
        ("wincred", "Windows Credential Manager"),
    ]
    .into_iter()
    .find(|(word, _)| {
        let words = helper.split(|c: char| !c.is_ascii_alphanumeric());
        words.into_iter().any(|found| found == *word)
    })
    .map(|(_, name)| name)
}

/// The `credential.helper` values that apply to `url`, in the order git tries
/// them: least specific config file first, and an empty value clearing the
/// list so far, as `gh auth setup-git` writes it.
pub(super) fn helpers_for(config: &Config, url: &str) -> Vec<String> {
    let mut helpers = Vec::new();
    let Ok(entries) = config.entries(Some(r"^credential\..*helper$")) else {
        return helpers;
    };
    let _ = entries.for_each(|entry| {
        let in_scope = entry.name().is_ok_and(|name| scope_covers(name, url));
        match entry.value() {
            Ok("") if in_scope => helpers.clear(),
            Ok(helper) if in_scope => helpers.push(helper.to_owned()),
            _ => {}
        }
    });
    helpers
}

/// Whether a config key like `credential.https://github.com.helper` covers
/// `url`. Plain `credential.helper` covers every URL.
fn scope_covers(key: &str, url: &str) -> bool {
    let scope = key
        .strip_prefix("credential.")
        .and_then(|key| key.strip_suffix(".helper"));
    match scope {
        Some(scope) => {
            let scope = scope.trim_end_matches('/');
            url.strip_prefix(scope)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
        }
        None => true,
    }
}

/// Runs one helper's `get`, returning the username and password it gave.
fn ask(helper: &str, url: &str, username: Option<&str>, limit: &Limit) -> Option<(String, String)> {
    let path = search_path(env::var_os("PATH"), &git_install_dirs());
    ask_on_path(helper, url, username, &path, limit)
}

fn ask_on_path(
    helper: &str,
    url: &str,
    username: Option<&str>,
    path: &OsStr,
    limit: &Limit,
) -> Option<(String, String)> {
    let mut command = shell(&format!("{} get", helper_command(helper)));
    let mut child = command
        .env("PATH", path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    child
        .stdin
        .take()?
        .write_all(request(url, username)?.as_bytes())
        .ok()?;
    if !exit_within(&mut child, limit)?.success() {
        return None;
    }
    let mut answer = Vec::new();
    child.stdout.take()?.read_to_end(&mut answer).ok()?;
    parse_answer(&String::from_utf8_lossy(&answer), username)
}

/// How `child` ended, or `None` once `limit` is up and it has been killed. An
/// answer is short enough to sit in the pipe until the helper exits.
fn exit_within(child: &mut Child, limit: &Limit) -> Option<ExitStatus> {
    loop {
        if let Some(status) = child.try_wait().ok()? {
            return Some(status);
        }
        if limit.is_up() {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        thread::sleep(HELPER_POLL);
    }
}

/// A helper's config value as a shell command. Git's three forms are a `!`
/// command, a path, and a bare name for `git-credential-<name>`, which
/// [`search_path`] finds.
fn helper_command(helper: &str) -> String {
    if let Some(command) = helper.strip_prefix('!') {
        command.to_owned()
    } else if std::path::Path::new(helper.trim_start_matches(['"', '\''])).is_absolute() {
        helper.to_owned()
    } else {
        format!("git-credential-{helper}")
    }
}

/// The `PATH` helpers run with. Helpers that ship with git live in its exec
/// path, which only `git --exec-path` reports, and an app opened from the
/// Finder has a short `PATH`, so the usual install folders are added after it.
fn search_path(inherited: Option<OsString>, install_dirs: &[PathBuf]) -> OsString {
    let inherited = inherited.unwrap_or_default();
    let dirs = env::split_paths(&inherited).chain(install_dirs.iter().cloned());
    env::join_paths(dirs).unwrap_or(inherited)
}

#[cfg(not(windows))]
fn git_install_dirs() -> Vec<PathBuf> {
    [
        "/opt/homebrew/bin",
        "/usr/local/bin",
        "/opt/homebrew/opt/git/libexec/git-core",
        "/usr/local/opt/git/libexec/git-core",
        "/Library/Developer/CommandLineTools/usr/libexec/git-core",
        "/Applications/Xcode.app/Contents/Developer/usr/libexec/git-core",
        "/usr/lib/git-core",
        "/usr/libexec/git-core",
    ]
    .map(PathBuf::from)
    .into()
}

/// Git for Windows, for all users or just this one. Its installer puts only
/// `Git\cmd` on the `PATH`, and that holds no credential helpers.
#[cfg(windows)]
fn git_install_dirs() -> Vec<PathBuf> {
    let program_files = env::var_os("ProgramFiles").map(PathBuf::from);
    let local = env::var_os("LOCALAPPDATA").map(|dir| PathBuf::from(dir).join("Programs"));
    [program_files, local]
        .into_iter()
        .flatten()
        .flat_map(|root| {
            let mingw = root.join("Git").join("mingw64");
            [mingw.join("bin"), mingw.join("libexec").join("git-core")]
        })
        .collect()
}

#[cfg(not(windows))]
fn shell(line: &str) -> Command {
    let mut command = Command::new("sh");
    command.args(["-c", line]);
    command
}

/// `cmd` stands in for the `sh` that only Git for Windows brings. It doesn't
/// read single quotes, which `gh auth setup-git` wraps around its path.
#[cfg(windows)]
fn shell(line: &str) -> Command {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let mut command = Command::new("cmd");
    command
        .raw_arg(format!("/S /C \"{}\"", line.replace('\'', "\"")))
        .creation_flags(CREATE_NO_WINDOW);
    command
}

/// The git credential protocol's request: `key=value` lines, then a blank one.
fn request(url: &str, username: Option<&str>) -> Option<String> {
    let url = Url::parse(url).ok()?;
    let mut host = url.host_str()?.to_owned();
    if let Some(port) = url.port() {
        host = format!("{host}:{port}");
    }
    let mut request = format!("protocol={}\nhost={host}\n", url.scheme());
    if let Some(username) = username {
        request.push_str(&format!("username={username}\n"));
    }
    request.push('\n');
    Some(request)
}

fn parse_answer(answer: &str, asked_as: Option<&str>) -> Option<(String, String)> {
    let field = |key: &str| {
        answer
            .lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
    };
    let username = field("username").or(asked_as)?;
    Some((username.to_owned(), field("password")?.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with(contents: &str) -> (tempfile::TempDir, Config) {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("gitconfig");
        std::fs::write(&path, contents).unwrap();
        let mut config = Config::new().unwrap();
        config
            .add_file(&path, git2::ConfigLevel::Global, false)
            .unwrap();
        (folder, config)
    }

    #[test]
    fn gh_setup_clears_the_general_helpers_for_github_only() {
        let (_folder, config) = config_with(concat!(
            "[credential]\n\thelper = osxkeychain\n",
            "[credential \"https://github.com\"]\n\thelper =\n",
            "\thelper = !/opt/homebrew/bin/gh auth git-credential\n",
        ));

        assert_eq!(
            helpers_for(&config, "https://github.com/Vyttle-LLC/mudling.git"),
            ["!/opt/homebrew/bin/gh auth git-credential"]
        );
        assert_eq!(
            helpers_for(&config, "https://github.community/a.git"),
            ["osxkeychain"]
        );
    }

    #[test]
    fn a_helper_is_named_only_if_known_and_never_quoted() {
        assert_eq!(
            plain_name("!gh auth git-credential"),
            Some("the GitHub CLI")
        );
        assert_eq!(plain_name("osxkeychain"), Some("the macOS keychain"));
        assert_eq!(plain_name("!f() { echo password=hunter2; }; f"), None);
    }

    #[test]
    fn helper_forms_become_commands_without_git() {
        assert_eq!(helper_command("manager"), "git-credential-manager");
        assert_eq!(
            helper_command("!gh auth git-credential"),
            "gh auth git-credential"
        );
        let absolute = std::env::temp_dir().join("helper");
        let absolute = absolute.to_str().unwrap();
        assert_eq!(helper_command(absolute), absolute);
    }

    #[test]
    fn the_search_path_keeps_the_players_folders_first() {
        let (first, second) = (PathBuf::from("first"), PathBuf::from("second"));
        let inherited = env::join_paths([&first]).unwrap();

        let path = search_path(Some(inherited), std::slice::from_ref(&second));

        assert_eq!(env::split_paths(&path).collect::<Vec<_>>(), [first, second]);
    }

    /// A helper that ships with git isn't on a Finder app's `PATH`.
    #[cfg(unix)]
    #[test]
    fn a_bare_helper_name_is_found_in_a_git_install_folder() {
        use std::os::unix::fs::PermissionsExt;

        let exec_path = tempfile::tempdir().unwrap();
        let helper = exec_path.path().join("git-credential-fake");
        let script = "#!/bin/sh\ncat >/dev/null\necho username=me\necho password=p\n";
        std::fs::write(&helper, script).unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
        let url = "https://example.com/r.git";
        let finder_path = OsString::from("/usr/bin:/bin");
        let install_dirs = [exec_path.path().to_owned()];

        let path = search_path(Some(finder_path.clone()), &install_dirs);

        let limit = Limit::after(Duration::from_secs(60));
        let answer = ask_on_path("fake", url, None, &path, &limit);
        assert_eq!(answer, Some(("me".to_owned(), "p".to_owned())));
        assert_eq!(ask_on_path("fake", url, None, &finder_path, &limit), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_hanging_helper_is_killed_when_the_limit_is_up() {
        let folder = tempfile::tempdir().unwrap();
        let pid_file = folder.path().join("pid");
        let helper = format!("!echo $$ > '{}'; exec sleep 600; ", pid_file.display());
        let url = "https://example.com/r.git";
        let limit = Limit::after(Duration::from_millis(200));

        let answer = ask(&helper, url, None, &limit);

        assert_eq!(answer, None);
        let pid = std::fs::read_to_string(pid_file).unwrap();
        let alive = Command::new("kill")
            .args(["-0", pid.trim()])
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(!alive.success(), "the helper should be gone");
    }

    #[test]
    fn the_request_names_the_host_and_port_only() {
        assert_eq!(
            request("https://git.example.com:8443/team/repo.git", Some("me")).unwrap(),
            "protocol=https\nhost=git.example.com:8443\nusername=me\n\n"
        );
    }

    #[test]
    fn an_answer_needs_a_password() {
        assert_eq!(
            parse_answer("username=me\npassword=a=b\n", None),
            Some(("me".to_owned(), "a=b".to_owned()))
        );
        assert_eq!(
            parse_answer("password=p\n", Some("asked")),
            Some(("asked".to_owned(), "p".to_owned()))
        );
        assert_eq!(parse_answer("username=me\n", None), None);
    }
}
