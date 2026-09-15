//! Where the permanent device credential lives.
//!
//! The credential the server hands back from `POST /api/devices/pair` is the
//! single most sensitive value this application holds: it is bearer proof
//! that this computer is a trusted device, and it does not expire.
//!
//! Rules enforced by this module:
//!
//! - It is written only to the OS secret store. On Windows that is
//!   **Windows Credential Manager**, reached through the `keyring` crate.
//! - It is never written to a file, a JSON blob, a config, or a log line.
//! - It is never returned across the Tauri IPC boundary. `commands` can ask
//!   this module to *use* a credential; it cannot ask for its value.
//! - It is namespaced per `serverId`, never one global key, so two Arciin
//!   servers can be paired independently and forgetting one leaves the other
//!   alone.

use crate::error::{from_code, AppError};

/// Windows Credential Manager target prefix. The `serverId` is appended, so
/// the stored target reads e.g.
/// `arciin-desktop:device-credential:9f1c...`.
const SERVICE_PREFIX: &str = "arciin-desktop:device-credential";

/// Target prefix for computer-backup credentials (`arcsync_...`).
///
/// Deliberately a different namespace from the device credential: they have
/// different lifetimes and different blast radii. Revoking the device kills
/// both; disabling backup kills only this one, and must not disturb pairing.
const SYNC_SERVICE_PREFIX: &str = "arciin-desktop:sync-credential";

/// The account field of the generic credential. The interesting part of the
/// identity is the service string; this stays constant.
const ACCOUNT: &str = "device";

/// Saving, loading and deleting a per-server device credential.
///
/// A trait rather than free functions so the pairing and connection flows can
/// be tested against an in-memory store, and so a macOS Keychain
/// implementation can be added later without touching callers.
pub trait CredentialStore: Send + Sync {
    fn save(&self, server_id: &str, secret: &str) -> Result<(), AppError>;
    fn load(&self, server_id: &str) -> Result<Option<String>, AppError>;
    fn delete(&self, server_id: &str) -> Result<(), AppError>;

    /// Store the computer-backup credential for one server *and* one backup
    /// profile.
    ///
    /// Keyed by both because a server can host several users, each with their
    /// own profile and their own grant. Sharing one slot would let one
    /// account's backup authorization be used under another's.
    fn save_sync(&self, server_id: &str, profile_id: &str, secret: &str) -> Result<(), AppError>;
    fn load_sync(&self, server_id: &str, profile_id: &str) -> Result<Option<String>, AppError>;
    fn delete_sync(&self, server_id: &str, profile_id: &str) -> Result<(), AppError>;
}

/// The real store. On Windows `keyring` resolves to the native
/// Credential Manager backend.
pub struct OsCredentialStore;

fn entry(server_id: &str) -> Result<keyring::Entry, AppError> {
    open_entry(&format!("{SERVICE_PREFIX}:{server_id}"))
}

fn sync_entry(server_id: &str, profile_id: &str) -> Result<keyring::Entry, AppError> {
    open_entry(&format!("{SYNC_SERVICE_PREFIX}:{server_id}:{profile_id}"))
}

fn open_entry(service: &str) -> Result<keyring::Entry, AppError> {
    keyring::Entry::new(service, ACCOUNT).map_err(|err| {
        // `err` names the target, not the secret.
        tracing::error!(error = %err, "credential store entry could not be opened");
        from_code("CREDENTIAL_STORE_ERROR")
    })
}

impl CredentialStore for OsCredentialStore {
    fn save(&self, server_id: &str, secret: &str) -> Result<(), AppError> {
        entry(server_id)?.set_password(secret).map_err(|err| {
            tracing::error!(error = %err, "credential could not be stored");
            from_code("CREDENTIAL_STORE_ERROR")
        })?;
        tracing::info!(server_id, "device credential stored in the OS secret store");
        Ok(())
    }

    fn load(&self, server_id: &str) -> Result<Option<String>, AppError> {
        match entry(server_id)?.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(err) => {
                tracing::error!(error = %err, "credential could not be read");
                Err(from_code("CREDENTIAL_STORE_ERROR"))
            }
        }
    }

    fn delete(&self, server_id: &str) -> Result<(), AppError> {
        match entry(server_id)?.delete_credential() {
            Ok(()) => {
                tracing::info!(
                    server_id,
                    "device credential removed from the OS secret store"
                );
                Ok(())
            }
            // Already gone is the state the caller wanted.
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => {
                tracing::error!(error = %err, "credential could not be deleted");
                Err(from_code("CREDENTIAL_STORE_ERROR"))
            }
        }
    }

    fn save_sync(&self, server_id: &str, profile_id: &str, secret: &str) -> Result<(), AppError> {
        sync_entry(server_id, profile_id)?
            .set_password(secret)
            .map_err(|err| {
                tracing::error!(error = %err, "sync credential could not be stored");
                from_code("CREDENTIAL_STORE_ERROR")
            })?;
        tracing::info!(
            server_id,
            profile_id,
            "backup credential stored in the OS secret store"
        );
        Ok(())
    }

    fn load_sync(&self, server_id: &str, profile_id: &str) -> Result<Option<String>, AppError> {
        match sync_entry(server_id, profile_id)?.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(err) => {
                tracing::error!(error = %err, "sync credential could not be read");
                Err(from_code("CREDENTIAL_STORE_ERROR"))
            }
        }
    }

    fn delete_sync(&self, server_id: &str, profile_id: &str) -> Result<(), AppError> {
        match sync_entry(server_id, profile_id)?.delete_credential() {
            Ok(()) => {
                tracing::info!(
                    server_id,
                    profile_id,
                    "backup credential removed from the OS secret store"
                );
                Ok(())
            }
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => {
                tracing::error!(error = %err, "sync credential could not be deleted");
                Err(from_code("CREDENTIAL_STORE_ERROR"))
            }
        }
    }
}

/// In-memory store used by tests. Never compiled into a release binary path.
#[cfg(test)]
pub struct MemoryCredentialStore {
    entries: std::sync::Mutex<std::collections::HashMap<String, String>>,
}

#[cfg(test)]
impl MemoryCredentialStore {
    pub fn new() -> Self {
        Self {
            entries: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }
}

#[cfg(test)]
impl CredentialStore for MemoryCredentialStore {
    fn save(&self, server_id: &str, secret: &str) -> Result<(), AppError> {
        self.entries
            .lock()
            .unwrap()
            .insert(server_id.to_string(), secret.to_string());
        Ok(())
    }

    fn load(&self, server_id: &str) -> Result<Option<String>, AppError> {
        Ok(self.entries.lock().unwrap().get(server_id).cloned())
    }

    fn delete(&self, server_id: &str) -> Result<(), AppError> {
        self.entries.lock().unwrap().remove(server_id);
        Ok(())
    }

    fn save_sync(&self, server_id: &str, profile_id: &str, secret: &str) -> Result<(), AppError> {
        self.entries
            .lock()
            .unwrap()
            .insert(format!("sync:{server_id}:{profile_id}"), secret.to_string());
        Ok(())
    }

    fn load_sync(&self, server_id: &str, profile_id: &str) -> Result<Option<String>, AppError> {
        Ok(self
            .entries
            .lock()
            .unwrap()
            .get(&format!("sync:{server_id}:{profile_id}"))
            .cloned())
    }

    fn delete_sync(&self, server_id: &str, profile_id: &str) -> Result<(), AppError> {
        self.entries
            .lock()
            .unwrap()
            .remove(&format!("sync:{server_id}:{profile_id}"));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SERVER_A: &str = "3f2504e0-4f89-41d3-9a0c-0305e82c3301";
    const SERVER_B: &str = "9c858901-8a57-4791-81fe-4c455b099bc9";

    #[test]
    fn a_credential_round_trips() {
        let store = MemoryCredentialStore::new();
        store.save(SERVER_A, "secret-a").unwrap();
        assert_eq!(store.load(SERVER_A).unwrap().as_deref(), Some("secret-a"));
    }

    #[test]
    fn an_unknown_server_has_no_credential() {
        let store = MemoryCredentialStore::new();
        assert_eq!(store.load(SERVER_A).unwrap(), None);
    }

    #[test]
    fn each_server_gets_its_own_key() {
        // The property that makes "forget one server" safe, and that stops a
        // reassigned LAN address from reaching another server's secret.
        let store = MemoryCredentialStore::new();
        store.save(SERVER_A, "secret-a").unwrap();
        store.save(SERVER_B, "secret-b").unwrap();

        assert_eq!(store.load(SERVER_A).unwrap().as_deref(), Some("secret-a"));
        assert_eq!(store.load(SERVER_B).unwrap().as_deref(), Some("secret-b"));
    }

    #[test]
    fn deleting_one_server_leaves_the_others_alone() {
        let store = MemoryCredentialStore::new();
        store.save(SERVER_A, "secret-a").unwrap();
        store.save(SERVER_B, "secret-b").unwrap();

        store.delete(SERVER_A).unwrap();

        assert_eq!(store.load(SERVER_A).unwrap(), None);
        assert_eq!(store.load(SERVER_B).unwrap().as_deref(), Some("secret-b"));
    }

    #[test]
    fn deleting_twice_is_not_an_error() {
        // Revocation handling calls this on a path that may already have run.
        let store = MemoryCredentialStore::new();
        store.save(SERVER_A, "secret-a").unwrap();
        store.delete(SERVER_A).unwrap();
        store.delete(SERVER_A).unwrap();
    }

    #[test]
    fn the_os_key_is_namespaced_per_server() {
        assert_ne!(
            format!("{SERVICE_PREFIX}:{SERVER_A}"),
            format!("{SERVICE_PREFIX}:{SERVER_B}"),
            "one global credential key would let servers read each other's secret"
        );
    }
}

#[cfg(test)]
mod sync_tests {
    use super::*;

    const SERVER_A: &str = "3f2504e0-4f89-41d3-9a0c-0305e82c3301";
    const SERVER_B: &str = "9c858901-8a57-4791-81fe-4c455b099bc9";

    #[test]
    fn a_sync_credential_round_trips() {
        let store = MemoryCredentialStore::new();
        store
            .save_sync(SERVER_A, "profile-1", "arcsync_abc")
            .unwrap();
        assert_eq!(
            store.load_sync(SERVER_A, "profile-1").unwrap().as_deref(),
            Some("arcsync_abc")
        );
    }

    #[test]
    fn device_and_sync_credentials_never_collide() {
        // They are different secrets with different lifetimes; one must not be
        // returned where the other was asked for.
        let store = MemoryCredentialStore::new();
        store.save(SERVER_A, "device-secret").unwrap();
        store
            .save_sync(SERVER_A, "profile-1", "arcsync_secret")
            .unwrap();

        assert_eq!(
            store.load(SERVER_A).unwrap().as_deref(),
            Some("device-secret")
        );
        assert_eq!(
            store.load_sync(SERVER_A, "profile-1").unwrap().as_deref(),
            Some("arcsync_secret")
        );
    }

    #[test]
    fn each_profile_gets_its_own_sync_credential() {
        // Two accounts on one server must not share a backup grant.
        let store = MemoryCredentialStore::new();
        store
            .save_sync(SERVER_A, "profile-1", "arcsync_one")
            .unwrap();
        store
            .save_sync(SERVER_A, "profile-2", "arcsync_two")
            .unwrap();

        assert_eq!(
            store.load_sync(SERVER_A, "profile-1").unwrap().as_deref(),
            Some("arcsync_one")
        );
        assert_eq!(
            store.load_sync(SERVER_A, "profile-2").unwrap().as_deref(),
            Some("arcsync_two")
        );
    }

    #[test]
    fn sync_credentials_are_isolated_across_servers() {
        let store = MemoryCredentialStore::new();
        store
            .save_sync(SERVER_A, "profile-1", "arcsync_home")
            .unwrap();
        store
            .save_sync(SERVER_B, "profile-1", "arcsync_office")
            .unwrap();

        assert_eq!(
            store.load_sync(SERVER_A, "profile-1").unwrap().as_deref(),
            Some("arcsync_home")
        );
        assert_eq!(
            store.load_sync(SERVER_B, "profile-1").unwrap().as_deref(),
            Some("arcsync_office")
        );
    }

    #[test]
    fn disabling_backup_leaves_pairing_intact() {
        // Protocol section 16: disabling backup drops the sync credential but
        // the computer stays paired.
        let store = MemoryCredentialStore::new();
        store.save(SERVER_A, "device-secret").unwrap();
        store
            .save_sync(SERVER_A, "profile-1", "arcsync_secret")
            .unwrap();

        store.delete_sync(SERVER_A, "profile-1").unwrap();

        assert!(store.load_sync(SERVER_A, "profile-1").unwrap().is_none());
        assert_eq!(
            store.load(SERVER_A).unwrap().as_deref(),
            Some("device-secret"),
            "device pairing must survive a backup disable"
        );
    }

    #[test]
    fn the_two_namespaces_are_distinct_targets() {
        assert_ne!(SERVICE_PREFIX, SYNC_SERVICE_PREFIX);
        assert!(!SYNC_SERVICE_PREFIX.starts_with(SERVICE_PREFIX));
    }

    // --- Identity, not address -------------------------------------------
    //
    // Home networks move: a router reboots, a lease changes, a machine swaps
    // Wi-Fi for Ethernet. The server is the same server, and a client that
    // re-keyed its secrets by address would ask its owner to pair again every
    // time — leaving another trusted device on the server each time it did.

    #[test]
    fn a_server_that_moved_keeps_its_device_credential() {
        // Nothing here mentions an address, which is the point: there is
        // nowhere for one to get in.
        let credentials = MemoryCredentialStore::new();
        credentials
            .save(SERVER_A, "device-credential-value")
            .unwrap();
        assert_eq!(
            credentials.load(SERVER_A).unwrap().as_deref(),
            Some("device-credential-value"),
            "the pairing must survive the server moving"
        );
    }

    #[test]
    fn a_server_that_moved_keeps_its_backup_grant() {
        let credentials = MemoryCredentialStore::new();
        credentials
            .save_sync(SERVER_A, "profile-1", "arcsync_example_for_this_test")
            .unwrap();
        assert!(
            credentials
                .load_sync(SERVER_A, "profile-1")
                .unwrap()
                .is_some(),
            "backup must not need re-authorising because the router rebooted"
        );
    }

    #[test]
    fn one_servers_credential_never_answers_for_another() {
        // The other half. Two instances must not be able to borrow each
        // other's trust, however similar their addresses.
        let credentials = MemoryCredentialStore::new();
        credentials.save(SERVER_A, "a-credential").unwrap();
        assert!(
            credentials.load(SERVER_B).unwrap().is_none(),
            "credentials are filed per identity, and must stay that way"
        );
    }
}
