//! The per-device cache of the vault key. [`Vault`] only ever sees the
//! [`KeyStore`] trait: the app passes [`device_key_store`], and tests and
//! benchmarks pass a [`MemoryKeyStore`], so they never reach the real Keychain.
//!
//! [`Vault`]: super::Vault

#[cfg(target_os = "macos")]
mod touch_id;

use super::secret::{KEY_LEN, VaultKey};
use keyring_core::Error as KeyringError;
use keyring_core::api::CredentialStoreApi as _;
use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};
use zeroize::Zeroizing;

/// The credential store's service name. Each vault is its own account.
const SERVICE: &str = "com.vyttle.mudling.vault";

/// Whether [`device_key_store`] guards the key: only a macOS release build
/// does, and only the release pipeline sets [`crate::RELEASE_VERSION`].
/// Everywhere else, whoever can use this computer can read the device's copy.
pub const DEVICE_KEY_GUARDED: bool = cfg!(target_os = "macos") && crate::RELEASE_VERSION.is_some();

/// The store this device keeps its vault keys in. A guarded one asks for
/// Touch ID, or the login password where there's none, before every read.
pub fn device_key_store() -> Arc<dyn KeyStore> {
    #[cfg(target_os = "macos")]
    if DEVICE_KEY_GUARDED {
        return Arc::new(touch_id::TouchIdKeyStore);
    }
    Arc::new(OsKeyStore)
}

/// A device's store of vault keys, by account. Calls may block on the OS or on
/// a system prompt; the vault only makes them from its own thread.
pub trait KeyStore: Send + Sync {
    /// The key, asking the player first if the store guards it. A prompt
    /// they cancel or fail gives [`KeyStoreError::Cancelled`].
    fn load(&self, account: &str) -> Result<Option<VaultKey>, KeyStoreError>;
    /// The key if reading it needs no prompt; a guarded store gives `None`.
    /// The vault reads a key it's replacing this way, so an unlock with the
    /// master password never asks for Touch ID or Face ID as well.
    fn load_without_prompt(&self, account: &str) -> Result<Option<VaultKey>, KeyStoreError> {
        self.load(account)
    }
    fn save(&self, account: &str, key: &VaultKey) -> Result<(), KeyStoreError>;
    /// Removes the key; removing one that isn't there succeeds.
    fn delete(&self, account: &str) -> Result<(), KeyStoreError>;
}

#[derive(Debug)]
pub enum KeyStoreError {
    /// The player cancelled or failed the prompt guarding the key.
    Cancelled,
    /// The OS credential store refused or failed a request.
    Failed(String),
}

impl fmt::Display for KeyStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => {
                f.write_str("the prompt for this device's key was cancelled or failed")
            }
            Self::Failed(reason) => write!(f, "the credential store failed: {reason}"),
        }
    }
}

impl std::error::Error for KeyStoreError {}

impl From<KeyringError> for KeyStoreError {
    fn from(error: KeyringError) -> Self {
        Self::Failed(error.to_string())
    }
}

/// Keychain on macOS and iOS, Credential Manager on Windows, Secret Service
/// on Linux, with nothing asked before a read.
#[derive(Debug, Default)]
pub struct OsKeyStore;

impl KeyStore for OsKeyStore {
    fn load(&self, account: &str) -> Result<Option<VaultKey>, KeyStoreError> {
        match entry(account)?.get_secret() {
            Ok(secret) => {
                // A value of the wrong size was never a key of ours.
                Ok(VaultKey::from_slice(&Zeroizing::new(secret)))
            }
            Err(KeyringError::NoEntry) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn save(&self, account: &str, key: &VaultKey) -> Result<(), KeyStoreError> {
        Ok(entry(account)?.set_secret(key.expose())?)
    }

    fn delete(&self, account: &str) -> Result<(), KeyStoreError> {
        match entry(account)?.delete_credential() {
            Ok(()) | Err(KeyringError::NoEntry) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

fn entry(account: &str) -> Result<keyring_core::Entry, KeyStoreError> {
    Ok(platform_store()?.build(SERVICE, account, None)?)
}

#[cfg(target_os = "macos")]
use apple_native_keyring_store::keychain::Store as PlatformStore;
#[cfg(target_os = "ios")]
use apple_native_keyring_store::protected::Store as PlatformStore;
#[cfg(target_os = "windows")]
use windows_native_keyring_store::Store as PlatformStore;
#[cfg(target_os = "linux")]
use zbus_secret_service_keyring_store::Store as PlatformStore;

fn platform_store() -> Result<Arc<PlatformStore>, KeyStoreError> {
    Ok(PlatformStore::new()?)
}

/// A store that lives and dies with the process. Tests and benchmarks use it;
/// it also suits a device with no working credential store.
#[derive(Debug, Default)]
pub struct MemoryKeyStore {
    keys: Mutex<HashMap<String, Zeroizing<[u8; KEY_LEN]>>>,
}

impl KeyStore for MemoryKeyStore {
    fn load(&self, account: &str) -> Result<Option<VaultKey>, KeyStoreError> {
        let keys = self.keys.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(keys
            .get(account)
            .and_then(|key| VaultKey::from_slice(key.as_slice())))
    }

    fn save(&self, account: &str, key: &VaultKey) -> Result<(), KeyStoreError> {
        let mut keys = self.keys.lock().unwrap_or_else(PoisonError::into_inner);
        keys.insert(account.to_owned(), Zeroizing::new(*key.expose()));
        Ok(())
    }

    fn delete(&self, account: &str) -> Result<(), KeyStoreError> {
        let mut keys = self.keys.lock().unwrap_or_else(PoisonError::into_inner);
        keys.remove(account);
        Ok(())
    }
}
