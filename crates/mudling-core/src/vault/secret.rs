//! Types that hold secrets: each wipes itself when dropped and prints as
//! `[redacted]`. None implements `Display`, `Clone` or `Serialize`, so a secret
//! reaches a log or a file only through a deliberate `expose`.

use super::error::Error;
use data_encoding::BASE32_NOPAD;
use std::fmt;
use zeroize::Zeroizing;

pub(super) const KEY_LEN: usize = 32;
/// Characters in each dash-separated group of a printed recovery key.
const RECOVERY_GROUP_LEN: usize = 4;

/// A password: the master password, or a saved server password.
pub struct Secret(Zeroizing<String>);

impl Secret {
    pub fn new(value: String) -> Self {
        Self(Zeroizing::new(value))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl From<String> for Secret {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl From<&str> for Secret {
    fn from(value: &str) -> Self {
        Self::new(value.to_owned())
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret([redacted])")
    }
}

/// The random key that encrypts every saved password. Only a [`KeyStore`]
/// implementation should ever need [`VaultKey::expose`].
///
/// [`KeyStore`]: super::KeyStore
pub struct VaultKey(Zeroizing<[u8; KEY_LEN]>);

impl VaultKey {
    pub(super) fn generate() -> Self {
        Self(random_key())
    }

    /// `None` unless `bytes` is exactly one key long.
    pub fn from_slice(bytes: &[u8]) -> Option<Self> {
        let mut key = Zeroizing::new([0; KEY_LEN]);
        (bytes.len() == KEY_LEN).then(|| {
            key.copy_from_slice(bytes);
            Self(key)
        })
    }

    pub fn expose(&self) -> &[u8; KEY_LEN] {
        &self.0
    }
}

impl fmt::Debug for VaultKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("VaultKey([redacted])")
    }
}

/// The key shown once when a vault is created, to print or keep in a password
/// manager. It reads as dash-separated groups of base32.
pub struct RecoveryKey(Zeroizing<[u8; KEY_LEN]>);

impl RecoveryKey {
    pub(super) fn generate() -> Self {
        Self(random_key())
    }

    /// Reads a key as the player typed it: case and separators don't matter.
    pub fn parse(text: &str) -> Result<Self, Error> {
        let mut typed = Zeroizing::new(String::with_capacity(text.len()));
        typed.extend(
            text.chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .map(|c| c.to_ascii_uppercase()),
        );
        let mut bytes = Zeroizing::new([0; KEY_LEN]);
        if BASE32_NOPAD.decode_len(typed.len()) != Ok(KEY_LEN) {
            return Err(Error::WrongRecoveryKey);
        }
        BASE32_NOPAD
            .decode_mut(typed.as_bytes(), bytes.as_mut_slice())
            .map_err(|_| Error::WrongRecoveryKey)?;
        Ok(Self(bytes))
    }

    /// The key as the player should write it down.
    pub fn reveal(&self) -> Zeroizing<String> {
        let plain = Zeroizing::new(BASE32_NOPAD.encode(self.0.as_slice()));
        let mut grouped = Zeroizing::new(String::with_capacity(plain.len() * 2));
        for (index, group) in plain.as_bytes().chunks(RECOVERY_GROUP_LEN).enumerate() {
            if index > 0 {
                grouped.push('-');
            }
            grouped.extend(group.iter().map(|&byte| char::from(byte)));
        }
        grouped
    }

    pub(super) fn bytes(&self) -> &[u8; KEY_LEN] {
        &self.0
    }
}

impl fmt::Debug for RecoveryKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RecoveryKey([redacted])")
    }
}

fn random_key() -> Zeroizing<[u8; KEY_LEN]> {
    let mut key = Zeroizing::new([0; KEY_LEN]);
    getrandom::fill(key.as_mut_slice()).expect("the OS provides randomness");
    key
}
