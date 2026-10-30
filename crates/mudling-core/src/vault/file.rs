//! `secrets.vault`, format 1: pretty-printed JSON in a fixed field order, with
//! LF endings and one line per saved password.
//!
//! ```json
//! {
//!   "format": "mudling-vault",
//!   "version": 1,
//!   "vault_id": "<16 bytes>",
//!   "key_fingerprint": "<16 bytes>",
//!   "kdf": {
//!     "memory_kib": 65536,
//!     "iterations": 3,
//!     "parallelism": 1
//!   },
//!   "salt": "<16 bytes>",
//!   "master_password": {
//!     "check": "<32 bytes>",
//!     "vault_key": "<24-byte nonce, sealed key, 16-byte tag>"
//!   },
//!   "recovery_key": {
//!     "check": "<32 bytes>",
//!     "vault_key": "<24-byte nonce, sealed key, 16-byte tag>"
//!   },
//!   "digest": "<32 bytes>",
//!   "entries": {
//!     "<32 hex digits>": "<24-byte nonce, sealed entry, 16-byte tag>"
//!   }
//! }
//! ```
//!
//! Binary values are base64. Argon2id, XChaCha20-Poly1305 and BLAKE2b are fixed
//! by the version number. Server and character names, like passwords, exist
//! only inside the entry ciphertexts.
//!
//! - **Diffs:** entries sort by id, and saving one rewrites only its line.
//! - **Damage:** `digest` covers everything that decides whether a password is
//!   right, so a damaged salt reads as corrupt instead of as a wrong password.
//!   The vault key's sealing and each entry's are authenticated, and an entry
//!   is bound to its id and vault key.
//! - **Merging:** there's deliberately no authentication across the whole
//!   entry list, because it would make every two-device edit conflict. So
//!   removing an entry's line, or restoring an older one, isn't detected.

use super::crypto::{
    self, CHECK_LEN, FINGERPRINT_LEN, KdfParams, SALT_LEN, VAULT_ID_LEN, WrappingKey,
};
use super::error::Error;
use super::secret::VaultKey;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write as _};
use std::path::Path;
use tempfile::NamedTempFile;

const FORMAT: &str = "mudling-vault";
const VERSION: u32 = 1;

/// Which wrapping of the vault key is being sealed or opened.
#[derive(Clone, Copy)]
pub(super) enum Wrapping {
    Master,
    Recovery,
}

#[derive(PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct VaultFile {
    format: String,
    version: u32,
    #[serde(with = "base64")]
    vault_id: [u8; VAULT_ID_LEN],
    #[serde(with = "base64")]
    pub key_fingerprint: [u8; FINGERPRINT_LEN],
    pub kdf: KdfParams,
    #[serde(with = "base64")]
    pub salt: [u8; SALT_LEN],
    master_password: WrappedKey,
    recovery_key: WrappedKey,
    #[serde(with = "base64")]
    digest: [u8; CHECK_LEN],
    pub entries: BTreeMap<String, Sealed>,
}

#[derive(PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WrappedKey {
    #[serde(with = "base64")]
    check: [u8; CHECK_LEN],
    vault_key: Sealed,
}

/// A nonce and ciphertext, as [`crypto::seal`] returns them.
#[derive(PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub(super) struct Sealed(#[serde(with = "base64")] pub Vec<u8>);

/// Just enough to pick a parser, so a newer vault reports its version instead
/// of failing on fields this build doesn't know.
#[derive(Deserialize)]
struct Header {
    format: String,
    version: u32,
}

impl VaultFile {
    /// Builds the file for a new vault key, with no entries yet.
    pub fn new(
        vault_id: [u8; VAULT_ID_LEN],
        key: &VaultKey,
        kdf: KdfParams,
        salt: [u8; SALT_LEN],
        master: &WrappingKey,
        recovery: &WrappingKey,
    ) -> Self {
        let wrapped_with = |wrapping: &WrappingKey| WrappedKey {
            check: wrapping.check,
            vault_key: Sealed(Vec::new()),
        };
        let mut file = Self {
            format: FORMAT.to_owned(),
            version: VERSION,
            vault_id,
            key_fingerprint: crypto::fingerprint(key),
            kdf,
            salt,
            master_password: wrapped_with(master),
            recovery_key: wrapped_with(recovery),
            digest: [0; CHECK_LEN],
            entries: BTreeMap::new(),
        };
        file.digest = crypto::digest(&file.header_bytes());
        file.master_password.vault_key = Sealed(crypto::seal(
            &master.key,
            key.expose(),
            &file.wrapping_aad(Wrapping::Master),
        ));
        file.recovery_key.vault_key = Sealed(crypto::seal(
            &recovery.key,
            key.expose(),
            &file.wrapping_aad(Wrapping::Recovery),
        ));
        file
    }

    pub fn vault_id(&self) -> [u8; VAULT_ID_LEN] {
        self.vault_id
    }

    /// Opens the vault key with `wrapping`, telling a key that doesn't belong
    /// to this vault from sealed data that's been altered.
    pub fn unwrap_key(&self, which: Wrapping, wrapping: &WrappingKey) -> Result<VaultKey, Error> {
        let (wrapped, wrong) = match which {
            Wrapping::Master => (&self.master_password, Error::WrongPassword),
            Wrapping::Recovery => (&self.recovery_key, Error::WrongRecoveryKey),
        };
        if wrapped.check != wrapping.check {
            return Err(wrong);
        }
        let plain = crypto::open(
            &wrapping.key,
            &wrapped.vault_key.0,
            &self.wrapping_aad(which),
        )
        .ok_or(Error::Tampered)?;
        VaultKey::from_slice(&plain).ok_or(Error::Tampered)
    }

    /// The associated data that binds an entry to its id and this vault key.
    pub fn entry_aad(&self, id: &str) -> Vec<u8> {
        let mut aad = FORMAT.as_bytes().to_vec();
        aad.extend_from_slice(b"/entry");
        aad.extend_from_slice(&self.vault_id);
        aad.extend_from_slice(&self.key_fingerprint);
        aad.extend_from_slice(id.as_bytes());
        aad
    }

    fn wrapping_aad(&self, which: Wrapping) -> Vec<u8> {
        let mut aad = self.header_bytes();
        aad.extend_from_slice(match which {
            Wrapping::Master => b"/master",
            Wrapping::Recovery => b"/recovery",
        });
        aad
    }

    /// Every header field except the sealed keys themselves, in a fixed
    /// layout that doesn't depend on how the JSON was spaced.
    fn header_bytes(&self) -> Vec<u8> {
        let mut bytes = FORMAT.as_bytes().to_vec();
        bytes.extend_from_slice(&self.version.to_le_bytes());
        bytes.extend_from_slice(&self.vault_id);
        bytes.extend_from_slice(&self.key_fingerprint);
        bytes.extend_from_slice(&self.kdf.memory_kib.to_le_bytes());
        bytes.extend_from_slice(&self.kdf.iterations.to_le_bytes());
        bytes.extend_from_slice(&self.kdf.parallelism.to_le_bytes());
        bytes.extend_from_slice(&self.salt);
        bytes.extend_from_slice(&self.master_password.check);
        bytes.extend_from_slice(&self.recovery_key.check);
        bytes
    }
}

/// Reads a vault's bytes, telling a short file from a damaged one.
pub(super) fn parse(bytes: &[u8]) -> Result<VaultFile, Error> {
    let header: Header = serde_json::from_slice(bytes).map_err(read_error)?;
    if header.format != FORMAT {
        return Err(Error::Corrupt("not a MUDling vault"));
    }
    if header.version != VERSION {
        return Err(Error::UnsupportedVersion(header.version));
    }
    let file: VaultFile = serde_json::from_slice(bytes).map_err(read_error)?;
    if !file.kdf.in_bounds() {
        return Err(Error::Corrupt("Argon2 parameters out of range"));
    }
    if crypto::digest(&file.header_bytes()) != file.digest {
        return Err(Error::Corrupt("the header doesn't match its digest"));
    }
    Ok(file)
}

/// `serde_json` reports end-of-input apart from other errors, which is what
/// separates a truncated write from corruption. Its messages can quote the
/// input, so none of them is kept.
fn read_error(error: serde_json::Error) -> Error {
    if error.is_eof() {
        Error::Truncated
    } else {
        Error::Corrupt("not valid vault JSON")
    }
}

/// Writes the whole file beside its destination, flushes it, then renames it
/// over the old one, so a crash leaves either the old vault or the new one.
pub(super) fn save(path: &Path, file: &VaultFile) -> io::Result<()> {
    let temporary = write_temporary(path, file)?;
    temporary.persist(path).map_err(|error| error.error)?;
    sync_folder(path)
}

/// Like [`save`], but a vault already at `path`, however recently it arrived,
/// stays as it is and gives [`Error::AlreadyExists`].
pub(super) fn save_new(path: &Path, file: &VaultFile) -> Result<(), Error> {
    let temporary = write_temporary(path, file)?;
    temporary
        .persist_noclobber(path)
        .map_err(|error| match error.error.kind() {
            io::ErrorKind::AlreadyExists => Error::AlreadyExists,
            _ => error.error.into(),
        })?;
    Ok(sync_folder(path)?)
}

fn write_temporary(path: &Path, file: &VaultFile) -> io::Result<NamedTempFile> {
    let mut text = serde_json::to_vec_pretty(file)?;
    text.push(b'\n');
    let folder = folder_of(path);
    fs::create_dir_all(folder)?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".secrets.vault.")
        .tempfile_in(folder)?;
    temporary.write_all(&text)?;
    temporary.as_file().sync_all()?;
    Ok(temporary)
}

/// A rename is only durable once its folder is; Windows can't open one.
fn sync_folder(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    fs::File::open(folder_of(path))?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn folder_of(path: &Path) -> &Path {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    }
}

/// Serde adapters for binary values stored as base64 text.
mod base64 {
    use data_encoding::BASE64;
    use serde::de::Error as _;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer, T: AsRef<[u8]>>(
        value: &T,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&BASE64.encode(value.as_ref()))
    }

    pub fn deserialize<'de, D: Deserializer<'de>, T: TryFrom<Vec<u8>>>(
        deserializer: D,
    ) -> Result<T, D::Error> {
        let text = <&str>::deserialize(deserializer)?;
        let bytes = BASE64
            .decode(text.as_bytes())
            .map_err(|_| D::Error::custom("invalid base64"))?;
        bytes
            .try_into()
            .map_err(|_| D::Error::custom("wrong length"))
    }
}
