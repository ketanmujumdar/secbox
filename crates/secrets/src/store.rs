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
    /// The names stored for this project, found without reading any value.
    /// `Ok(None)` where the store has no way to list them.
    fn names(&self) -> Result<Option<Vec<SecretName>>, StoreError> {
        Ok(None)
    }
}

/// Splits a keychain account, `<project id>/<secret name>`, into its parts.
/// `None` for an account that is not one of ours.
pub fn split_account(account: &str) -> Option<(ProjectId, SecretName)> {
    let (project, name) = account.split_once('/')?;
    Some((ProjectId::new(project).ok()?, SecretName::new(name).ok()?))
}

/// The accounts under the product's keychain service, each
/// `<project id>/<secret name>`, found from their attributes only: no value
/// is read, so the keychain does not ask for the login password. `Ok(None)`
/// where this platform's store cannot list them.
pub fn keychain_accounts() -> Result<Option<Vec<String>>, StoreError> {
    list_accounts(brand::KEYCHAIN_SERVICE)
}

#[cfg(target_os = "macos")]
fn list_accounts(service: &str) -> Result<Option<Vec<String>>, StoreError> {
    use security_framework::item::{ItemClass, ItemSearchOptions, Limit};
    /// `errSecItemNotFound`: nothing is stored under the service.
    const NOT_FOUND: i32 = -25300;
    let found = ItemSearchOptions::new()
        .class(ItemClass::generic_password())
        .service(service)
        .load_attributes(true)
        .load_data(false)
        .limit(Limit::All)
        .search();
    match found {
        Ok(items) => {
            let mut accounts: Vec<String> = items
                .iter()
                .filter_map(|item| item.simplify_dict())
                .filter_map(|attributes| attributes.get("acct").cloned())
                .collect();
            accounts.sort();
            accounts.dedup();
            Ok(Some(accounts))
        }
        Err(error) if error.code() == NOT_FOUND => Ok(Some(Vec::new())),
        Err(_) => Err(StoreError::Unavailable),
    }
}

#[cfg(not(target_os = "macos"))]
fn list_accounts(_service: &str) -> Result<Option<Vec<String>>, StoreError> {
    Ok(None)
}

/// The names among `accounts` that belong to `project`.
fn names_for(accounts: &[String], project: &ProjectId) -> Vec<SecretName> {
    accounts
        .iter()
        .filter_map(|account| split_account(account))
        .filter(|(owner, _)| owner == project)
        .map(|(_, name)| name)
        .collect()
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

    fn names(&self) -> Result<Option<Vec<SecretName>>, StoreError> {
        let values = self.values.lock().map_err(|_| StoreError::Unavailable)?;
        let mut names: Vec<SecretName> = values.keys().cloned().collect();
        names.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        Ok(Some(names))
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

    fn names(&self) -> Result<Option<Vec<SecretName>>, StoreError> {
        Ok(list_accounts(&self.service)?.map(|accounts| names_for(&accounts, &self.project)))
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
    fn an_account_splits_into_project_and_name_and_others_are_left_out() {
        let (owner, name) = split_account("proj-1/API_KEY").unwrap();
        assert_eq!(owner.as_str(), "proj-1");
        assert_eq!(name.as_str(), "API_KEY");
        assert!(split_account("no-slash").is_none());
        let accounts = vec![
            "proj-1/API_KEY".to_owned(),
            "proj-10/OTHER".to_owned(),
            "proj-1/DB_URL".to_owned(),
            "someone-else".to_owned(),
        ];
        let names: Vec<String> = names_for(&accounts, &project()).iter().map(|n| n.as_str().to_owned()).collect();
        assert_eq!(names, ["API_KEY", "DB_URL"]);
    }

    #[test]
    fn a_memory_store_lists_its_names() {
        let store = MemoryStore::new();
        assert_eq!(store.names().unwrap().unwrap().len(), 0);
        store.put(&SecretName::new("B").unwrap(), &SecretValue::new("x")).unwrap();
        store.put(&SecretName::new("A").unwrap(), &SecretValue::new("y")).unwrap();
        let names: Vec<String> = store.names().unwrap().unwrap().iter().map(|n| n.as_str().to_owned()).collect();
        assert_eq!(names, ["A", "B"]);
    }

    #[test]
    fn keyring_error_mapping_drops_backend_bytes() {
        let leaked = b"hunter2-value".to_vec();
        let mapped = map_keyring_error(keyring::Error::BadEncoding(leaked));
        assert_eq!(mapped, StoreError::BadEncoding);
        assert!(!format!("{mapped} {mapped:?}").contains("hunter2"));
    }

    /// Lists the real keychain's accounts, attributes only. Reads no value, so
    /// it should never prompt, but it touches the real keychain.
    #[test]
    #[ignore = "lists the real OS keychain's accounts"]
    fn keychain_accounts_are_listed_without_values() {
        let accounts = keychain_accounts().unwrap();
        if cfg!(target_os = "macos") {
            let accounts = accounts.unwrap();
            assert!(accounts.iter().all(|a| !a.is_empty()));
            println!("{} accounts", accounts.len());
        }
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
