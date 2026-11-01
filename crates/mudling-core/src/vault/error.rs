//! Everything the vault can report. Messages name what went wrong and never
//! quote the file, a key or a password.

use super::key_store::KeyStoreError;
use std::fmt;
use std::io;

#[derive(Debug)]
pub enum Error {
    WrongPassword,
    /// The recovery key doesn't open this vault, or isn't a recovery key.
    WrongRecoveryKey,
    /// The file ends early: a write that stopped partway.
    Truncated,
    /// The file is complete but isn't a valid vault.
    Corrupt(&'static str),
    /// The file parses, but encrypted data fails authentication.
    Tampered,
    /// The file was written by a newer MUDling.
    UnsupportedVersion(u32),
    /// The strength check refused a new master password.
    WeakPassword {
        warning: Option<String>,
    },
    NotCreated,
    /// A vault file already exists, and creating one would overwrite it.
    AlreadyExists,
    /// The file changed while the master password was being changed, usually
    /// because a sync landed. Nothing was written, so it's safe to try again.
    ChangedDuringSave,
    /// No readable password is saved to keep, so settling a conflict that way
    /// would drop the only copy. Nothing was written.
    NoCurrentPassword,
    /// The vault's thread found it locked. [`Vault`] waits on this rather
    /// than returning it.
    ///
    /// [`Vault`]: super::Vault
    Locked,
    /// This device holds no cached key for this vault, so unlocking takes the
    /// master password or the recovery key.
    NotCached,
    Stopped,
    Io(io::Error),
    KeyStore(KeyStoreError),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongPassword => f.write_str("wrong master password"),
            Self::WrongRecoveryKey => f.write_str("wrong recovery key"),
            Self::Truncated => f.write_str("the vault file ends early"),
            Self::Corrupt(reason) => write!(f, "the vault file is corrupt: {reason}"),
            Self::Tampered => f.write_str("the vault file failed authentication"),
            Self::UnsupportedVersion(version) => {
                write!(f, "vault format {version} needs a newer MUDling")
            }
            Self::WeakPassword { warning: None } => f.write_str("that master password is too weak"),
            Self::WeakPassword {
                warning: Some(warning),
            } => write!(f, "that master password is too weak: {warning}"),
            Self::NotCreated => f.write_str("no vault has been created"),
            Self::AlreadyExists => f.write_str("a vault already exists"),
            Self::ChangedDuringSave => f.write_str("the vault file changed; try again"),
            Self::NoCurrentPassword => f.write_str("no current password is saved to keep"),
            Self::Locked => f.write_str("the vault is locked"),
            Self::NotCached => f.write_str("this device has no cached vault key"),
            Self::Stopped => f.write_str("the vault's thread has stopped"),
            Self::Io(error) => write!(f, "vault file: {error}"),
            Self::KeyStore(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::KeyStore(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<KeyStoreError> for Error {
    fn from(error: KeyStoreError) -> Self {
        Self::KeyStore(error)
    }
}
