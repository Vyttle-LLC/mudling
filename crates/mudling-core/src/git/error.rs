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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitError {
    /// The remote couldn't be reached. Retry when the network returns.
    NoNetwork(String),
    /// The remote refused this machine's credentials, or nothing on this
    /// machine could sign in. Retrying won't help until the player acts.
    AuthRefused(String),
    /// The remote has commits this device lacks. Fetch, merge and push again.
    NonFastForward(String),
    /// Another process holds the repo. Retry shortly.
    Locked(String),
    /// The chosen folder holds files and isn't a git repo.
    FolderNotEmpty,
    Failed(String),
}

impl fmt::Display for GitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoNetwork(detail) => write!(f, "can't reach the remote: {detail}"),
            Self::AuthRefused(detail) => write!(f, "sign-in refused: {detail}"),
            Self::NonFastForward(detail) => write!(f, "the remote is ahead: {detail}"),
            Self::Locked(detail) => write!(f, "the repo is locked: {detail}"),
            Self::FolderNotEmpty => f.write_str("the folder holds files and isn't a git repo"),
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
            // libgit2 has no code for this refusal, only the message.
            (_, ErrorClass::Http) if detail.contains("403") => Self::AuthRefused(detail),
            (_, ErrorClass::Os)
                if OS_NETWORK_FAILURES
                    .iter()
                    .any(|prefix| detail.starts_with(prefix)) =>
            {
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
