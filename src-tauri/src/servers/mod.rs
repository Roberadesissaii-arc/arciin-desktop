//! Remembering which Arciin servers this computer knows about.
//!
//! This file is plain JSON in the app config directory and holds **no
//! secrets**. The device credential for each server lives in Windows
//! Credential Manager (see `crate::credentials`); all that is kept here is
//! enough to recognise a server again and show it on a card.
//!
//! The shape is a list, not a single record, so more than one server can be
//! paired even though V1 connects to one at a time.

use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::error::{from_code, AppError};

const STORE_FILE: &str = "servers.json";

/// Non-secret metadata for one known server.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SavedServer {
    /// Stable public id from the discovery manifest. The primary key, and the
    /// namespace under which the credential is stored.
    pub server_id: String,
    /// Display name from the manifest, e.g. "Arciin Home".
    pub name: String,
    /// Canonical origin: `http://192.168.1.50`.
    pub base_url: String,
    /// Protocol version last agreed with this server.
    pub protocol_version: u32,
    /// RFC 3339, or `None` if it has never connected since being saved.
    #[serde(default)]
    pub last_connected_at: Option<String>,
    /// Set when the server told us this device was revoked. Kept (rather than
    /// deleting the row) so the UI can explain what happened instead of the
    /// server silently vanishing from the list.
    #[serde(default)]
    pub revoked: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct StoreFile {
    #[serde(default)]
    servers: Vec<SavedServer>,
}

/// Reads and writes `servers.json`, serialised so concurrent commands cannot
/// interleave a read-modify-write.
pub struct ServerStore {
    path: PathBuf,
    lock: Mutex<()>,
}

impl ServerStore {
    pub fn new(app: &AppHandle) -> Result<Self, AppError> {
        let dir = app.path().app_config_dir().map_err(|err| {
            tracing::error!(error = %err, "no app config directory");
            AppError::internal("Arciin Desktop could not find its settings folder.")
        })?;
        std::fs::create_dir_all(&dir).map_err(|err| {
            tracing::error!(error = %err, "settings folder could not be created");
            AppError::internal("Arciin Desktop could not create its settings folder.")
        })?;
        Ok(Self {
            path: dir.join(STORE_FILE),
            lock: Mutex::new(()),
        })
    }

    fn read_unlocked(&self) -> StoreFile {
        let Ok(raw) = std::fs::read(&self.path) else {
            return StoreFile::default();
        };
        // A corrupt or hand-edited file must not brick the app: start over
        // rather than refusing to launch. The credentials are unaffected.
        serde_json::from_slice(&raw).unwrap_or_else(|err| {
            tracing::warn!(error = %err, "servers.json was unreadable; starting empty");
            StoreFile::default()
        })
    }

    fn write_unlocked(&self, file: &StoreFile) -> Result<(), AppError> {
        let json = serde_json::to_vec_pretty(file)
            .map_err(|_| AppError::internal("Saved servers could not be serialised."))?;
        // Write-then-rename so an interrupted write cannot truncate the list.
        let temp = self.path.with_extension("json.tmp");
        std::fs::write(&temp, &json).map_err(|err| {
            tracing::error!(error = %err, "servers.json could not be written");
            AppError::internal("Saved servers could not be written.")
        })?;
        std::fs::rename(&temp, &self.path).map_err(|err| {
            tracing::error!(error = %err, "servers.json could not be replaced");
            AppError::internal("Saved servers could not be written.")
        })
    }

    pub fn list(&self) -> Vec<SavedServer> {
        let _guard = self.lock.lock().unwrap();
        self.read_unlocked().servers
    }

    pub fn get(&self, server_id: &str) -> Option<SavedServer> {
        let _guard = self.lock.lock().unwrap();
        self.read_unlocked()
            .servers
            .into_iter()
            .find(|server| server.server_id == server_id)
    }

    /// Insert or replace by `serverId`.
    pub fn upsert(&self, server: SavedServer) -> Result<(), AppError> {
        let _guard = self.lock.lock().unwrap();
        let mut file = self.read_unlocked();
        match file
            .servers
            .iter_mut()
            .find(|existing| existing.server_id == server.server_id)
        {
            Some(existing) => *existing = server,
            None => file.servers.push(server),
        }
        self.write_unlocked(&file)
    }

    /// Apply a change to one saved server, if it is present.
    pub fn update<F>(&self, server_id: &str, apply: F) -> Result<(), AppError>
    where
        F: FnOnce(&mut SavedServer),
    {
        let _guard = self.lock.lock().unwrap();
        let mut file = self.read_unlocked();
        let Some(server) = file
            .servers
            .iter_mut()
            .find(|existing| existing.server_id == server_id)
        else {
            return Err(from_code("SERVER_NOT_SAVED"));
        };
        apply(server);
        self.write_unlocked(&file)
    }

    /// Drop one server's metadata. Only this server: the rest are untouched.
    pub fn remove(&self, server_id: &str) -> Result<(), AppError> {
        let _guard = self.lock.lock().unwrap();
        let mut file = self.read_unlocked();
        file.servers.retain(|server| server.server_id != server_id);
        self.write_unlocked(&file)
    }
}
