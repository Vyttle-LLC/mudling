//! Why a git operation failed, in the terms sync decides by. Nothing here
//! means data was lost: everything is committed locally before a push is
//! tried, so sync can delay and retry (PRD §10).

use git2::{ErrorClass, ErrorCode};
use std::{fmt, io};

/// How libgit2 words a lost connection that it classes as an OS error: the
/// Unix socket stream's connect, then WinHTTP's steps.
const OS_NETWORK_FAILURES: [&str; 4] = [
    "failed to connect",
    "failed to send request",
    "failed to receive response",
    "failed to read data",
];

/// How libgit2 words an HTTP status it has no use for: its own client, then
/// WinHTTP.
const HTTP_STATUS_ANSWERS: [&str; 2] = [
    "unexpected http status code",
    "request failed with status code",
];

/// How libgit2 words a git server's error packet.
const SERVER_ERROR_PACKET: &str = "remote error:";

/// Said to the player when the folder chosen for sync holds MUDling's own data.
const HOLDS_THE_DATA_FOLDER: &str = "That folder holds MUDling's own data folder, which keeps databases and logs that never sync. Choose a different folder, such as a new one beside it.";

/// An HTTP status that says "not now": too many requests, or the server's own
/// trouble. Every 5xx counts, because each of them can pass.
fn is_passing_status(detail: &str) -> bool {
    let status = detail.rsplit(' ').next().unwrap_or_default();
    status == "429" || (status.len() == 3 && status.starts_with('5'))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitError {
    /// The remote couldn't be reached. Retry when the network returns.
    NoNetwork(String),
    /// The server was reached and said no: the repo is missing, say, or this
    /// key is read-only. Retrying won't help until the player acts.
    RemoteRefused(String),
    /// The server is having trouble, or wants fewer requests: an HTTP 5xx or
    /// 429. It can pass, so retry slowly and tell the player.
    Unavailable(String),
    /// A network step went past its [`Limit`](super::Limit): a stalled server,
    /// say, or a credential helper that never answered. Retry later.
    TimedOut(String),
    /// The remote refused this machine's credentials, or nothing on this
    /// machine could sign in. Retrying won't help until the player acts.
    AuthRefused(String),
    /// The remote has commits this device lacks. Fetch, merge and push again.
    NonFastForward(String),
    /// Another process holds the repo. Retry shortly.
    Locked(String),
    /// The chosen folder holds files and isn't a git repo.
    FolderNotEmpty,
    /// The folder is MUDling's data folder, or holds it, so the repo would
    /// commit the databases and logs that stay on this device.
    HoldsTheDataFolder,
    /// A file about to be pushed seems to hold a password, so nothing was
    /// pushed. `field` says where in the file; the value itself is never
    /// repeated. Nothing moves until the player takes it out.
    PasswordInFile {
        path: String,
        field: String,
    },
    /// A mirror's download or folder went past its cap, so it was stopped.
    TooLarge(String),
    Failed(String),
}

impl fmt::Display for GitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoNetwork(detail) => write!(f, "can't reach the remote: {detail}"),
            Self::RemoteRefused(detail) => write!(f, "the remote refused: {detail}"),
            Self::Unavailable(detail) => write!(f, "the remote is having trouble: {detail}"),
            Self::TimedOut(detail) => write!(f, "gave up waiting: {detail}"),
            Self::AuthRefused(detail) => write!(f, "sign-in refused: {detail}"),
            Self::NonFastForward(detail) => write!(f, "the remote is ahead: {detail}"),
            Self::Locked(detail) => write!(f, "the repo is locked: {detail}"),
            Self::FolderNotEmpty => f.write_str("the folder holds files and isn't a git repo"),
            Self::HoldsTheDataFolder => f.write_str(HOLDS_THE_DATA_FOLDER),
            Self::PasswordInFile { path, field } => {
                write!(f, "not pushed: {path} seems to hold a password in {field}")
            }
            Self::TooLarge(detail) => write!(f, "it's too large for a plugin: {detail}"),
            Self::Failed(detail) => f.write_str(detail),
        }
    }
}

impl std::error::Error for GitError {}

impl From<git2::Error> for GitError {
    fn from(error: git2::Error) -> Self {
        let detail = error.message().to_owned();
        match (error.code(), error.class()) {
            (ErrorCode::Auth | ErrorCode::Certificate, _) => Self::AuthRefused(detail),
            (ErrorCode::NotFastForward, _) => Self::NonFastForward(detail),
            (ErrorCode::Locked, _) => Self::Locked(detail),
            // libgit2 has no codes for these refusals, only the messages.
            (_, ErrorClass::Http) if starts_with_any(&detail, &HTTP_STATUS_ANSWERS) => {
                if detail.ends_with("403") {
                    Self::AuthRefused(detail)
                } else if is_passing_status(&detail) {
                    Self::Unavailable(detail)
                } else {
                    Self::RemoteRefused(detail)
                }
            }
            (_, ErrorClass::Net) if detail.starts_with(SERVER_ERROR_PACKET) => {
                Self::RemoteRefused(detail)
            }
            // What an SSH server wrote to stderr, as libgit2 reports it.
            (ErrorCode::Eof, ErrorClass::Ssh) => Self::RemoteRefused(detail),
            (_, ErrorClass::Os) if starts_with_any(&detail, &OS_NETWORK_FAILURES) => {
                Self::NoNetwork(detail)
            }
            (_, ErrorClass::Net | ErrorClass::Http | ErrorClass::Ssh | ErrorClass::Ssl) => {
                Self::NoNetwork(detail)
            }
            _ => Self::Failed(detail),
        }
    }
}

impl From<io::Error> for GitError {
    fn from(error: io::Error) -> Self {
        Self::Failed(error.to_string())
    }
}

fn starts_with_any(detail: &str, prefixes: &[&str]) -> bool {
    prefixes.iter().any(|prefix| detail.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classify(code: ErrorCode, class: ErrorClass, message: &str) -> GitError {
        git2::Error::new(code, class, message).into()
    }

    #[test]
    fn an_ssh_servers_answer_is_a_refusal_but_a_dropped_session_is_not() {
        let missing_repo = classify(
            ErrorCode::Eof,
            ErrorClass::Ssh,
            "fatal: '/nope.git' does not appear to be a git repository\n",
        );
        assert!(matches!(missing_repo, GitError::RemoteRefused(_)));

        let dropped = classify(
            ErrorCode::GenericError,
            ErrorClass::Ssh,
            "SSH could not read data: Timed out waiting on socket",
        );
        assert!(matches!(dropped, GitError::NoNetwork(_)));
    }

    #[test]
    fn a_git_servers_error_packet_is_a_refusal() {
        let read_only = classify(
            ErrorCode::GenericError,
            ErrorClass::Net,
            "remote error: this key is read only",
        );
        assert!(matches!(read_only, GitError::RemoteRefused(_)));
    }

    #[test]
    fn winhttps_wording_of_a_status_is_read_too() {
        let not_found = classify(
            ErrorCode::GenericError,
            ErrorClass::Http,
            "request failed with status code: 404",
        );
        assert!(matches!(not_found, GitError::RemoteRefused(_)));

        let forbidden = classify(
            ErrorCode::GenericError,
            ErrorClass::Http,
            "request failed with status code: 403",
        );
        assert!(matches!(forbidden, GitError::AuthRefused(_)));
    }

    #[test]
    fn a_servers_passing_trouble_is_told_from_a_refusal() {
        for status in ["500", "502", "503", "504", "429"] {
            let answer = classify(
                ErrorCode::GenericError,
                ErrorClass::Http,
                &format!("unexpected http status code: {status}"),
            );
            assert!(matches!(answer, GitError::Unavailable(_)), "{status}");
        }
        let missing = classify(
            ErrorCode::GenericError,
            ErrorClass::Http,
            "unexpected http status code: 404",
        );
        assert!(matches!(missing, GitError::RemoteRefused(_)));
    }

    #[test]
    fn a_lost_connection_stays_retryable() {
        let lost = classify(
            ErrorCode::GenericError,
            ErrorClass::Net,
            "error receiving data from socket: Connection reset by peer",
        );
        assert!(matches!(lost, GitError::NoNetwork(_)));
    }
}
