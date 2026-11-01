//! Finds the ssh-agent that holds this machine's keys.
//!
//! An app opened from the Finder inherits launchd's `SSH_AUTH_SOCK`, an empty
//! agent, even when 1Password's agent holds the player's keys. libssh2 reads
//! only that variable, so when another agent is the one with keys, the variable
//! is pointed at it, once, at startup: see [`prepare`](super::prepare). On
//! Windows every agent shares one named pipe, which libssh2 finds itself.

use directories::BaseDirs;
use std::env;
use std::ffi::OsStr;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const AUTH_SOCK: &str = "SSH_AUTH_SOCK";
/// How long all the agents together may take. This runs before an app's first
/// window, and a healthy agent answers within milliseconds, so one that's hung
/// or waiting on a locked app is passed over rather than waited for.
const PROBE_BUDGET: Duration = Duration::from_millis(100);
const REQUEST_IDENTITIES: u8 = 11;
const IDENTITIES_ANSWER: u8 = 12;

/// Points libssh2 at the first agent with keys. With none, it stays put, and
/// signing in fails as refused.
pub(super) fn select() {
    let deadline = Instant::now() + PROBE_BUDGET;
    let found = agent_sockets()
        .into_iter()
        .find(|socket| has_keys(socket, deadline));
    let Some(agent) = found else {
        return;
    };
    if env::var_os(AUTH_SOCK).as_deref() != Some(agent.as_os_str()) {
        point_libssh2_at(&agent);
    }
}

#[allow(unsafe_code)]
fn point_libssh2_at(agent: &Path) {
    // SAFETY: `prepare`'s caller runs this before any other thread exists, so
    // nothing can read the environment at this instant. The variable is the
    // only way to give libssh2 a socket through git2.
    unsafe { env::set_var(AUTH_SOCK, agent) };
}

/// The player's own agent first, then 1Password's: its macOS default, then the
/// path it uses on Linux, which its macOS guide also suggests linking.
fn agent_sockets() -> Vec<PathBuf> {
    let configured = env::var_os(AUTH_SOCK).filter(|socket| !OsStr::is_empty(socket));
    let one_password = BaseDirs::new().into_iter().flat_map(|dirs| {
        let home = dirs.home_dir().to_owned();
        [
            home.join("Library/Group Containers/2BUA8C4S2C.com.1password/t/agent.sock"),
            home.join(".1password/agent.sock"),
        ]
    });
    configured
        .map(PathBuf::from)
        .into_iter()
        .chain(one_password)
        .collect()
}

fn has_keys(socket: &Path, deadline: Instant) -> bool {
    identity_count(socket, deadline).is_ok_and(|count| count > 0)
}

/// Asks an agent how many keys it holds, in the ssh-agent protocol: a
/// big-endian length, a message type, then for this answer the key count.
fn identity_count(socket: &Path, deadline: Instant) -> io::Result<u32> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err(io::ErrorKind::TimedOut.into());
    }
    let mut agent = UnixStream::connect(socket)?;
    agent.set_read_timeout(Some(left))?;
    agent.write_all(&[0, 0, 0, 1, REQUEST_IDENTITIES])?;
    let mut answer = [0; 9];
    agent.read_exact(&mut answer)?;
    let [_, _, _, _, kind, count @ ..] = answer;
    if kind == IDENTITIES_ANSWER {
        Ok(u32::from_be_bytes(count))
    } else {
        Err(io::ErrorKind::InvalidData.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;
    use std::thread;

    fn soon() -> Instant {
        Instant::now() + Duration::from_secs(5)
    }

    /// An agent that answers one request, holding `keys` keys.
    fn fake_agent(socket: &Path, keys: u32) {
        let listener = UnixListener::bind(socket).unwrap();
        thread::spawn(move || {
            let (mut client, _) = listener.accept().unwrap();
            let mut request = [0; 5];
            client.read_exact(&mut request).unwrap();
            assert_eq!(request[4], REQUEST_IDENTITIES);
            let mut answer = vec![0, 0, 0, 5, IDENTITIES_ANSWER];
            answer.extend(keys.to_be_bytes());
            client.write_all(&answer).unwrap();
        });
    }

    #[test]
    fn an_agent_with_keys_is_told_from_an_empty_one() {
        let folder = tempfile::tempdir().unwrap();
        let (empty, loaded) = (folder.path().join("e"), folder.path().join("l"));
        fake_agent(&empty, 0);
        fake_agent(&loaded, 2);

        assert!(!has_keys(&empty, soon()));
        assert!(has_keys(&loaded, soon()));
    }

    #[test]
    fn a_dead_socket_has_no_keys() {
        let folder = tempfile::tempdir().unwrap();
        assert!(!has_keys(&folder.path().join("gone"), soon()));
    }

    #[test]
    fn an_agent_that_never_answers_costs_only_the_time_left() {
        let folder = tempfile::tempdir().unwrap();
        let socket = folder.path().join("silent");
        let _listening = UnixListener::bind(&socket).unwrap();

        let started = Instant::now();
        let deadline = started + PROBE_BUDGET;
        assert!(!has_keys(&socket, deadline));
        assert!(!has_keys(&socket, deadline), "nothing is left to wait for");
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
