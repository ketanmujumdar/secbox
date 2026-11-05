use std::collections::HashMap;
use std::sync::Mutex;

use zeroize::{Zeroize, Zeroizing};

use crate::names::{ProjectId, SecretName};
use crate::value::SecretValue;

/// Failure from a secret store. Holds no value and no text from the backend,
/// since a backend error can embed stored bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum StoreError {
    #[error("the secret store could not be reached or refused access")]
    Unavailable,
    #[error("the stored secret is not valid UTF-8")]
    BadEncoding,
    #[error("the secret name or value is too long for the store")]
    TooLong,
    #[error("the secret store rejected the request")]
    Rejected,
}

/// Where secret values live.
pub trait SecretStore: Send + Sync {
    /// Stores a value, replacing any earlier one.
    fn put(&self, name: &SecretName, value: &SecretValue) -> Result<(), StoreError>;
    /// Reads a value. `Ok(None)` means there is none under that name.
    fn get(&self, name: &SecretName) -> Result<Option<SecretValue>, StoreError>;
    /// Deletes a value and reports whether one existed.
    fn delete(&self, name: &SecretName) -> Result<bool, StoreError>;
}

/// An in-memory store, for tests and for sandboxes with no persistent secrets.
#[derive(Default)]
pub struct MemoryStore {
    values: Mutex<HashMap<SecretName, SecretValue>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl SecretStore for MemoryStore {
    fn put(&self, name: &SecretName, value: &SecretValue) -> Result<(), StoreError> {
        let mut values = self.values.lock().map_err(|_| StoreError::Unavailable)?;
        values.insert(name.clone(), SecretValue::new(value.expose()));
        Ok(())
    }

    fn get(&self, name: &SecretName) -> Result<Option<SecretValue>, StoreError> {
        let values = self.values.lock().map_err(|_| StoreError::Unavailable)?;
        Ok(values.get(name).map(|v| SecretValue::new(v.expose())))
    }

    fn delete(&self, name: &SecretName) -> Result<bool, StoreError> {
        let mut values = self.values.lock().map_err(|_| StoreError::Unavailable)?;
        Ok(values.remove(name).is_some())
    }
}

/// A store in the OS keychain: macOS Keychain or Linux Secret Service.
///
/// Values go to the OS through the keyring library's native API, never through
/// a helper process or its command line. Entries use the product's keychain
/// service name, with `<project id>/<secret name>` as the account.
pub struct KeyringStore {
    service: String,
    project: ProjectId,
}

impl KeyringStore {
    /// Creates a handle. This does not touch the keychain.
    pub fn new(project: ProjectId) -> Self {
        Self::with_service(brand::KEYCHAIN_SERVICE, project)
    }

    /// Exists so that the ignored keychain test can use a throwaway service.
    fn with_service(service: &str, project: ProjectId) -> Self {
        Self {
            service: service.to_owned(),
            project,
        }
    }

    /// The keychain service this store reads and writes.
    pub fn service(&self) -> &str {
        &self.service
    }

    /// The keychain account for a secret.
    pub fn account(&self, name: &SecretName) -> String {
        format!("{}/{}", self.project, name)
    }

    fn entry(&self, name: &SecretName) -> Result<keyring::Entry, StoreError> {
        keyring::Entry::new(&self.service, &self.account(name)).map_err(map_keyring_error)
    }
}

/// Maps a backend error to a variant with no backend text. `BadEncoding` in
/// particular carries the stored bytes, so those are wiped here.
fn map_keyring_error(error: keyring::Error) -> StoreError {
    match error {
        keyring::Error::BadEncoding(mut bytes) => {
            bytes.zeroize();
            StoreError::BadEncoding
        }
        keyring::Error::TooLong(..) => StoreError::TooLong,
        keyring::Error::PlatformFailure(_) | keyring::Error::NoStorageAccess(_) => {
            StoreError::Unavailable
        }
        _ => StoreError::Rejected,
    }
}

impl SecretStore for KeyringStore {
    fn put(&self, name: &SecretName, value: &SecretValue) -> Result<(), StoreError> {
        self.entry(name)?
            .set_secret(value.expose().as_bytes())
            .map_err(map_keyring_error)
    }

    fn get(&self, name: &SecretName) -> Result<Option<SecretValue>, StoreError> {
        match self.entry(name)?.get_secret() {
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(text) => Ok(Some(SecretValue::from_zeroizing(Zeroizing::new(text)))),
                Err(error) => {
                    let _wiped = Zeroizing::new(error.into_bytes());
                    Err(StoreError::BadEncoding)
                }
            },
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(map_keyring_error(error)),
        }
    }

    fn delete(&self, name: &SecretName) -> Result<bool, StoreError> {
        match self.entry(name)?.delete_credential() {
            Ok(()) => Ok(true),
            Err(keyring::Error::NoEntry) => Ok(false),
            Err(error) => Err(map_keyring_error(error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> ProjectId {
        ProjectId::new("proj-1").unwrap()
    }

    #[test]
    fn keyring_store_uses_brand_service_and_project_account_without_touching_keychain() {
        let store = KeyringStore::new(project());
        assert_eq!(store.service(), brand::KEYCHAIN_SERVICE);
        let name = SecretName::new("API_KEY").unwrap();
        assert_eq!(store.account(&name), "proj-1/API_KEY");
    }

    #[test]
    fn keyring_error_mapping_drops_backend_bytes() {
        let leaked = b"hunter2-value".to_vec();
        let mapped = map_keyring_error(keyring::Error::BadEncoding(leaked));
        assert_eq!(mapped, StoreError::BadEncoding);
        assert!(!format!("{mapped} {mapped:?}").contains("hunter2"));
    }

    /// Talks to the real OS keychain under a throwaway service. It can prompt
    /// the user, so it is ignored by default.
    #[test]
    #[ignore = "uses the real OS keychain and may prompt the user"]
    fn keyring_round_trip_with_test_service() {
        let suffix = crate::Placeholder::generate().unwrap();
        let service = format!("{}-test-{}", brand::KEYCHAIN_SERVICE, suffix.as_str());
        let store = KeyringStore::with_service(&service, project());
        let name = SecretName::new("ROUND_TRIP").unwrap();
        store.put(&name, &SecretValue::new("test-only")).unwrap();
        let read = store.get(&name).unwrap().unwrap();
        assert_eq!(read.expose(), "test-only");
        assert!(store.delete(&name).unwrap());
        assert!(store.get(&name).unwrap().is_none());
    }
}
