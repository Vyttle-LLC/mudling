//! A macOS release build's store: Keychain items that only Touch ID, or the
//! login password where there's no Touch ID, can read. The Keychain enforces
//! that through each item's access control; MUDling checks nothing itself.
//!
//! These items live in the data protection Keychain, which only an app
//! signed with the access group in `apple/MUDling.entitlements`, and
//! the provisioning profile that allows it, can reach.
//! Any other build gets `errSecMissingEntitlement` and keeps no copy, so
//! every unlock takes the master password. It never falls back to an
//! unguarded copy.

use super::{KeyStore, KeyStoreError, OsKeyStore, SERVICE};
use crate::vault::secret::VaultKey;
use apple_native_keyring_store::protected::Store;
use keyring_core::Error as KeyringError;
use keyring_core::api::CredentialStoreApi as _;
use std::collections::HashMap;
use std::sync::Once;
use zeroize::Zeroizing;

const ERR_USER_CANCELED: i32 = -128;
const ERR_AUTH_FAILED: i32 = -25293;
const ERR_MISSING_ENTITLEMENT: i32 = -34018;

#[derive(Debug, Default)]
pub struct TouchIdKeyStore;

impl KeyStore for TouchIdKeyStore {
    fn load(&self, account: &str) -> Result<Option<VaultKey>, KeyStoreError> {
        match entry(account)?.get_secret() {
            Ok(secret) => Ok(VaultKey::from_slice(&Zeroizing::new(secret))),
            Err(KeyringError::NoEntry) => Ok(None),
            Err(error) => Err(failure(error)),
        }
    }

    fn load_without_prompt(&self, _: &str) -> Result<Option<VaultKey>, KeyStoreError> {
        Ok(None)
    }

    /// Replacing an item in place would ask for Touch ID, so the old one goes
    /// first, along with any unguarded copy this app can remove.
    fn save(&self, account: &str, key: &VaultKey) -> Result<(), KeyStoreError> {
        self.delete(account)?;
        entry(account)?.set_secret(key.expose()).map_err(failure)
    }

    /// The Keychain only lets the app that made an unguarded copy delete it,
    /// so one a development build left stays, never read, until that build
    /// or Keychain Access removes it.
    fn delete(&self, account: &str) -> Result<(), KeyStoreError> {
        let _ = OsKeyStore.delete(account);
        match entry(account)?.delete_credential() {
            Ok(()) | Err(KeyringError::NoEntry) => Ok(()),
            // A build that can't reach this Keychain has no copy there.
            Err(error) if code(&error) == Some(ERR_MISSING_ENTITLEMENT) => Ok(()),
            Err(error) => Err(failure(error)),
        }
    }
}

fn code(error: &KeyringError) -> Option<i32> {
    match error {
        KeyringError::PlatformFailure(inner) | KeyringError::NoStorageAccess(inner) => inner
            .downcast_ref::<security_framework::base::Error>()
            .map(|error| error.code()),
        _ => None,
    }
}

fn entry(account: &str) -> Result<keyring_core::Entry, KeyStoreError> {
    let guarded = HashMap::from([("access-policy", "require-user-presence")]);
    Ok(Store::new()?.build(SERVICE, account, Some(&guarded))?)
}

fn failure(error: KeyringError) -> KeyStoreError {
    static UNSIGNED: Once = Once::new();
    match code(&error) {
        Some(ERR_USER_CANCELED | ERR_AUTH_FAILED) => KeyStoreError::Cancelled,
        Some(ERR_MISSING_ENTITLEMENT) => {
            UNSIGNED.call_once(|| {
                eprintln!(
                    "this build isn't signed for the Keychain, so it keeps no copy of the \
                     vault key and every unlock takes the master password"
                );
            });
            error.into()
        }
        _ => error.into(),
    }
}
