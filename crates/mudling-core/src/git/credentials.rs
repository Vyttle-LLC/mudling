//! Signs in with what the machine already has (PRD §10): ssh-agent for SSH
//! remotes, gitconfig credential helpers for HTTPS. MUDling stores nothing.
//!
//! This runs helpers itself because `git2::Cred::credential_helper` shells out
//! to `git credential-<name>`, and no code path may need a system git.

use git2::{Config, Cred, CredentialType, ErrorClass, ErrorCode, RemoteCallbacks};
use std::io::Write;
use std::process::{Command, Stdio};
use url::Url;

/// libgit2 asks again after every refusal, so each way in is offered once.
pub(super) fn machine_callbacks<'a>(config: Config) -> RemoteCallbacks<'a> {
    let mut agent_offered = false;
    let mut helpers: Option<std::vec::IntoIter<String>> = None;
    let mut callbacks = RemoteCallbacks::new();
    callbacks.credentials(move |url, username, allowed| {
        if allowed.contains(CredentialType::USERNAME) {
            return Cred::username(username.unwrap_or("git"));
        }
        if allowed.contains(CredentialType::SSH_KEY) && !agent_offered {
            agent_offered = true;
            #[cfg(unix)]
            super::ssh_agent::select();
            return Cred::ssh_key_from_agent(username.unwrap_or("git"));
        }
        if allowed.contains(CredentialType::USER_PASS_PLAINTEXT) {
            let remaining = helpers.get_or_insert_with(|| helpers_for(&config, url).into_iter());
            if let Some((user, password)) = remaining.find_map(|h| ask(&h, url, username)) {
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

/// The `credential.helper` values that apply to `url`, in the order git tries
/// them: least specific config file first, and an empty value clearing the
/// list so far, as `gh auth setup-git` writes it.
fn helpers_for(config: &Config, url: &str) -> Vec<String> {
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
fn ask(helper: &str, url: &str, username: Option<&str>) -> Option<(String, String)> {
    let mut command = shell(&format!("{} get", helper_command(helper)));
    let mut child = command
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
    let output = child.wait_with_output().ok()?;
    if !output.status.success() {
        return None;
    }
    parse_answer(&String::from_utf8_lossy(&output.stdout), username)
}

/// A helper's config value as a shell command. Git's three forms are a `!`
/// command, a path, and a bare name for `git-credential-<name>` on the `PATH`.
fn helper_command(helper: &str) -> String {
    if let Some(command) = helper.strip_prefix('!') {
        command.to_owned()
    } else if std::path::Path::new(helper.trim_start_matches(['"', '\''])).is_absolute() {
        helper.to_owned()
    } else {
        format!("git-credential-{helper}")
    }
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
