//! The vault's cryptography, with no I/O: key derivation, sealing, and the
//! encoding of a saved password inside its ciphertext.

use super::EntryKey;
use super::error::Error;
use super::secret::{KEY_LEN, RecoveryKey, Secret, VaultKey};
use argon2::{Algorithm, Argon2, Params, Version};
use blake2::digest::{KeyInit as _, Mac as _};
use blake2::{Blake2b512, Blake2bMac512, Digest as _};
use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use data_encoding::HEXLOWER;
use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization as _;
use zeroize::Zeroizing;

pub(super) const SALT_LEN: usize = 16;
pub(super) const CHECK_LEN: usize = 32;
pub(super) const VAULT_ID_LEN: usize = 16;
pub(super) const FINGERPRINT_LEN: usize = 16;
const NONCE_LEN: usize = 24;
const ENTRY_ID_LEN: usize = 16;
/// Plaintext is padded to a multiple of this, so a ciphertext's length says
/// nothing about how long a password or server name is.
const ENTRY_BLOCK: usize = 256;

const RECOVERY_DOMAIN: &[u8] = b"mudling-vault/recovery";
const FINGERPRINT_DOMAIN: &[u8] = b"mudling-vault/fingerprint";
const ENTRY_ID_DOMAIN: &[u8] = b"mudling-vault/entry-id";

/// Argon2id's cost. It's stored with the salt, so a vault keeps opening after
/// [`KdfParams::DEFAULT`] changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct KdfParams {
    pub memory_kib: u32,
    pub iterations: u32,
    pub parallelism: u32,
}

impl KdfParams {
    /// RFC 9106's memory-constrained choice of 64 MiB and three passes, kept
    /// to one lane so its cost is the same on every device.
    pub const DEFAULT: Self = Self {
        memory_kib: 64 * 1024,
        iterations: 3,
        parallelism: 1,
    };

    /// Above these limits a file is corrupt or hostile, not slow: one edited
    /// number must not make a phone allocate gigabytes.
    pub const MAX_MEMORY_KIB: u32 = 1024 * 1024;
    pub const MAX_ITERATIONS: u32 = 32;
    pub const MAX_PARALLELISM: u32 = 16;

    pub fn in_bounds(&self) -> bool {
        self.memory_kib <= Self::MAX_MEMORY_KIB
            && self.iterations <= Self::MAX_ITERATIONS
            && self.parallelism <= Self::MAX_PARALLELISM
    }

    fn argon2(&self) -> Result<Argon2<'static>, Error> {
        let params = Params::new(
            self.memory_kib,
            self.iterations,
            self.parallelism,
            Some(WRAPPING_OUTPUT_LEN),
        )
        .map_err(|_| Error::Corrupt("Argon2 parameters out of range"))?;
        Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, params))
    }
}

const WRAPPING_OUTPUT_LEN: usize = KEY_LEN + CHECK_LEN;

/// What a master password or recovery key turns into: half encrypts the vault
/// key, and the other half is stored so a wrong password can be told from a
/// damaged file. Storing it costs nothing, because reaching it takes the same
/// Argon2 work as trying the password.
pub(super) struct WrappingKey {
    pub key: Zeroizing<[u8; KEY_LEN]>,
    pub check: [u8; CHECK_LEN],
}

impl WrappingKey {
    fn from_output(output: &[u8; WRAPPING_OUTPUT_LEN]) -> Self {
        let (key_half, check) = output.split_at(KEY_LEN);
        let mut key = Zeroizing::new([0; KEY_LEN]);
        key.copy_from_slice(key_half);
        Self {
            key,
            check: check.try_into().expect("the rest is CHECK_LEN"),
        }
    }

    /// The slow path: this is the Argon2 work the module docs describe.
    pub fn from_password(
        password: &Secret,
        salt: &[u8; SALT_LEN],
        params: KdfParams,
    ) -> Result<Self, Error> {
        // The same password can arrive composed or decomposed depending on the
        // device's keyboard, and must derive the same key on both.
        let mut normalized = Zeroizing::new(String::with_capacity(password.expose().len() * 3));
        normalized.extend(password.expose().nfc());
        let mut output = Zeroizing::new([0; WRAPPING_OUTPUT_LEN]);
        params
            .argon2()?
            .hash_password_into(normalized.as_bytes(), salt, output.as_mut_slice())
            .map_err(|_| Error::Corrupt("Argon2 refused its parameters"))?;
        Ok(Self::from_output(&output))
    }

    /// A recovery key is already 256 random bits, so it needs no slow hash.
    pub fn from_recovery_key(key: &RecoveryKey) -> Self {
        let output = Zeroizing::new(
            Blake2b512::new()
                .chain_update(RECOVERY_DOMAIN)
                .chain_update(key.bytes())
                .finalize()
                .into(),
        );
        Self::from_output(&output)
    }
}

pub(super) fn random_array<const N: usize>() -> [u8; N] {
    let mut bytes = [0; N];
    getrandom::fill(&mut bytes).expect("the OS provides randomness");
    bytes
}

/// Encrypts `plaintext` under a fresh random nonce, returned in front of the
/// ciphertext. XChaCha20's 24-byte nonce makes random nonces safe.
pub(super) fn seal(key: &[u8; KEY_LEN], plaintext: &[u8], aad: &[u8]) -> Vec<u8> {
    let nonce: [u8; NONCE_LEN] = random_array();
    let ciphertext = cipher(key)
        .encrypt(
            &XNonce::from(nonce),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .expect("a vault entry fits XChaCha20's message limit");
    [nonce.as_slice(), ciphertext.as_slice()].concat()
}

/// The reverse of [`seal`]; `None` when `sealed` fails authentication.
pub(super) fn open(key: &[u8; KEY_LEN], sealed: &[u8], aad: &[u8]) -> Option<Zeroizing<Vec<u8>>> {
    let (nonce, ciphertext) = sealed.split_first_chunk::<NONCE_LEN>()?;
    let payload = Payload {
        msg: ciphertext,
        aad,
    };
    cipher(key)
        .decrypt(&XNonce::from(*nonce), payload)
        .ok()
        .map(Zeroizing::new)
}

fn cipher(key: &[u8; KEY_LEN]) -> XChaCha20Poly1305 {
    let key: &Key = key.into();
    XChaCha20Poly1305::new(key)
}

/// A public name for a vault key, so a device can tell its cached key has
/// gone stale after the master password changed elsewhere.
pub(super) fn fingerprint(key: &VaultKey) -> [u8; FINGERPRINT_LEN] {
    let mut mac = vault_mac(key);
    mac.update(FINGERPRINT_DOMAIN);
    truncated(mac)
}

/// Where a saved password lives in the file. It's a keyed hash of the name:
/// repeatable, so two devices saving the same character write the same record,
/// and meaningless to anyone without the vault key.
pub(super) fn entry_id(key: &VaultKey, entry: &EntryKey) -> String {
    let mut mac = vault_mac(key);
    mac.update(ENTRY_ID_DOMAIN);
    for field in [&entry.server, &entry.character] {
        mac.update(&(field.len() as u64).to_le_bytes());
        mac.update(field.as_bytes());
    }
    HEXLOWER.encode(&truncated::<ENTRY_ID_LEN>(mac))
}

fn vault_mac(key: &VaultKey) -> Blake2bMac512 {
    Blake2bMac512::new_from_slice(key.expose()).expect("BLAKE2b accepts a 32-byte key")
}

fn truncated<const N: usize>(mac: Blake2bMac512) -> [u8; N] {
    let output = mac.finalize().into_bytes();
    output[..N].try_into().expect("N is under BLAKE2b's output")
}

/// A digest with no key: it catches accidental damage, not an attacker.
pub(super) fn digest(bytes: &[u8]) -> [u8; CHECK_LEN] {
    let output = Blake2b512::digest(bytes);
    output[..CHECK_LEN].try_into().expect("half of the output")
}

/// Lays out a name and password as three length-prefixed fields, padded to a
/// whole number of [`ENTRY_BLOCK`]s. The buffer is sized up front because a
/// `Vec` that grows leaves old copies behind that can't be wiped.
pub(super) fn encode_entry(entry: &EntryKey, password: &Secret) -> Zeroizing<Vec<u8>> {
    let fields = [
        entry.server.as_bytes(),
        entry.character.as_bytes(),
        password.expose().as_bytes(),
    ];
    let used: usize = fields.iter().map(|field| field.len() + 8).sum();
    let padded = used.next_multiple_of(ENTRY_BLOCK);
    let mut plaintext = Zeroizing::new(Vec::with_capacity(padded));
    for field in fields {
        plaintext.extend_from_slice(&(field.len() as u64).to_le_bytes());
        plaintext.extend_from_slice(field);
    }
    plaintext.resize(padded, 0);
    plaintext
}

/// The reverse of [`encode_entry`]. It only runs on authenticated plaintext,
/// so `None` means a bug or an unknown writer, not an attacker.
pub(super) fn decode_entry(plaintext: &[u8]) -> Option<(EntryKey, Secret)> {
    let mut rest = plaintext;
    let mut next_field = || {
        let (length, tail) = rest.split_first_chunk::<8>()?;
        let length = usize::try_from(u64::from_le_bytes(*length)).ok()?;
        let (field, tail) = tail.split_at_checked(length)?;
        rest = tail;
        std::str::from_utf8(field).ok()
    };
    let server = next_field()?.to_owned();
    let character = next_field()?.to_owned();
    let password = Secret::from(next_field()?);
    Some((EntryKey { server, character }, password))
}
