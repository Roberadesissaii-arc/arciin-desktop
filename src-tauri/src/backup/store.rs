//! Durable local sync state.
//!
//! This is the memory the backup engine needs to survive a restart: which
//! entries exist, what identity each one has, and which operations were in
//! flight when the process stopped. Without it, a relaunch mid-backup would
//! start from zero and duplicate everything already uploaded.
//!
//! # What is deliberately *not* in here
//!
//! No device credential, no `arcsync_` credential, no session cookie, no
//! password. Those live in Windows Credential Manager and the WebView's cookie
//! jar. This file is plain SQLite on disk and is treated as readable by anyone
//! who can read the user's profile — so it holds only metadata that would be
//! unremarkable if seen.
//!
//! Local absolute paths *are* stored, because the engine cannot watch a folder
//! it cannot name. They never leave the machine: what the server receives is
//! the opaque `sourcePathIdentifier` and a root-relative path.
//!
//! # Namespacing
//!
//! Everything is keyed by `server_id`, so a home server and an office server
//! never share state, and a second computer builds its own tree.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::{params, Connection, OptionalExtension};

use crate::error::AppError;

/// Bumped whenever the schema changes shape. Migration is forward-only.
// 2 added `profiles.status`: backup can now be turned off server-side while
// this computer stays paired, and the row has to outlive that so the folders
// can be offered back.
const SCHEMA_VERSION: i64 = 2;

/// What the engine intends to do, or has done, with one entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncState {
    /// Seen locally, nothing sent yet.
    Pending,
    /// An operation is in flight. On restart these are reconciled, never
    /// assumed complete.
    InProgress,
    /// The server acknowledged the current local content.
    Synced,
    /// Sending failed in a way worth retrying.
    Failed,
    /// Gone locally; the server has been told, or is about to be.
    Tombstoned,
}

impl SyncState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "PENDING",
            Self::InProgress => "IN_PROGRESS",
            Self::Synced => "SYNCED",
            Self::Failed => "FAILED",
            Self::Tombstoned => "TOMBSTONED",
        }
    }

    fn parse(raw: &str) -> Self {
        match raw {
            "IN_PROGRESS" => Self::InProgress,
            "SYNCED" => Self::Synced,
            "FAILED" => Self::Failed,
            "TOMBSTONED" => Self::Tombstoned,
            _ => Self::Pending,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryType {
    File,
    Folder,
}

impl EntryType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::File => "FILE",
            Self::Folder => "FOLDER",
        }
    }

    fn parse(raw: &str) -> Self {
        if raw == "FOLDER" {
            Self::Folder
        } else {
            Self::File
        }
    }
}

/// One tracked file or folder.
#[derive(Debug, Clone)]
pub struct Entry {
    /// Stable UUID. Survives rename and move.
    pub client_entry_id: String,
    pub root_id: String,
    pub relative_path: String,
    pub entry_type: EntryType,
    pub size_bytes: i64,
    /// Windows modified time, as a Unix timestamp in milliseconds.
    pub modified_ms: i64,
    pub state: SyncState,
    /// The operation currently in flight, if any. Reused verbatim on retry so
    /// the server replays rather than duplicates.
    pub pending_operation_id: Option<String>,
}

/// A protected root as this machine knows it.
#[derive(Debug, Clone)]
pub struct Root {
    /// The server's `SyncRoot.id`.
    pub id: String,
    pub kind: String,
    pub display_name: String,
    /// Local, never sent.
    pub local_path: PathBuf,
    pub enabled: bool,
}

/// The backup profile this machine holds for one server.
#[derive(Debug, Clone)]
pub struct Profile {
    pub server_id: String,
    pub profile_id: String,
    pub device_id: String,
    pub user_id: String,
    pub paused: bool,
    /// Whether the server still has backup switched on for this computer.
    ///
    /// A disabled profile is kept rather than deleted: the folders are still
    /// stored on the server, and the only way to offer them back is to
    /// remember which ones they were.
    pub enabled: bool,
}

/// SQLite-backed sync state, serialised behind one connection.
///
/// One writer is enough: the engine's concurrency is in its transfers, not in
/// its bookkeeping, and a single connection removes a whole class of locking
/// bugs for state that is written far more often than it is read.
pub struct SyncStore {
    conn: Mutex<Connection>,
}

impl SyncStore {
    /// Open (creating if needed) the sync database in the app's data folder.
    pub fn open(dir: &Path) -> Result<Self, AppError> {
        std::fs::create_dir_all(dir).map_err(|err| {
            tracing::error!(error = %err, "sync state folder could not be created");
            AppError::internal("Backup state could not be opened.")
        })?;
        let conn = Connection::open(dir.join("backup-state.db")).map_err(|err| {
            tracing::error!(error = %err, "sync database could not be opened");
            AppError::internal("Backup state could not be opened.")
        })?;

        // WAL survives an abrupt process exit far better than the rollback
        // journal, which matters because this database is written continuously
        // during a long backup.
        conn.pragma_update(None, "journal_mode", "WAL").ok();
        conn.pragma_update(None, "synchronous", "NORMAL").ok();
        conn.pragma_update(None, "foreign_keys", "ON").ok();

        let store = Self {
            conn: Mutex::new(conn),
        };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<(), AppError> {
        let conn = self.conn.lock().unwrap();
        let current: i64 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap_or(0);

        if current >= SCHEMA_VERSION {
            return Ok(());
        }

        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS profiles (
                server_id   TEXT PRIMARY KEY,
                profile_id  TEXT NOT NULL,
                device_id   TEXT NOT NULL,
                user_id     TEXT NOT NULL,
                paused      INTEGER NOT NULL DEFAULT 0,
                enabled     INTEGER NOT NULL DEFAULT 1,
                created_at  TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS roots (
                id           TEXT NOT NULL,
                server_id    TEXT NOT NULL,
                kind         TEXT NOT NULL,
                display_name TEXT NOT NULL,
                local_path   TEXT NOT NULL,
                enabled      INTEGER NOT NULL DEFAULT 1,
                PRIMARY KEY (server_id, id),
                FOREIGN KEY (server_id) REFERENCES profiles(server_id) ON DELETE CASCADE
            );

            CREATE TABLE IF NOT EXISTS entries (
                client_entry_id      TEXT NOT NULL,
                server_id            TEXT NOT NULL,
                root_id              TEXT NOT NULL,
                relative_path        TEXT NOT NULL,
                -- Lowercased path, because Windows cannot distinguish
                -- Report.pdf from report.pdf and neither should our identity.
                path_key             TEXT NOT NULL,
                entry_type           TEXT NOT NULL,
                size_bytes           INTEGER NOT NULL DEFAULT 0,
                modified_ms          INTEGER NOT NULL DEFAULT 0,
                state                TEXT NOT NULL,
                pending_operation_id TEXT,
                updated_at           TEXT NOT NULL,
                PRIMARY KEY (server_id, client_entry_id)
            );

            -- One logical entry per path within a root.
            CREATE UNIQUE INDEX IF NOT EXISTS entries_path
                ON entries (server_id, root_id, path_key);

            CREATE INDEX IF NOT EXISTS entries_state
                ON entries (server_id, state);
            "#,
        )
        .map_err(|err| {
            tracing::error!(error = %err, "sync schema could not be created");
            AppError::internal("Backup state could not be prepared.")
        })?;

        // Databases created at version 1 have the tables but not the column.
        // `CREATE TABLE IF NOT EXISTS` above is a no-op for them, so the
        // column has to be added separately.
        let has_enabled = conn.prepare("SELECT enabled FROM profiles LIMIT 0").is_ok();
        if !has_enabled {
            conn.execute_batch(
                "ALTER TABLE profiles ADD COLUMN enabled INTEGER NOT NULL DEFAULT 1;",
            )
            .map_err(|err| {
                tracing::error!(error = %err, "sync schema could not be upgraded");
                AppError::internal("Backup state could not be prepared.")
            })?;
            tracing::info!("sync schema upgraded: profiles.enabled added");
        }

        conn.pragma_update(None, "user_version", SCHEMA_VERSION)
            .ok();
        tracing::info!(version = SCHEMA_VERSION, "sync schema ready");
        Ok(())
    }

    // --- Profile ---------------------------------------------------------

    pub fn save_profile(&self, profile: &Profile) -> Result<(), AppError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO profiles
                (server_id, profile_id, device_id, user_id, paused, enabled, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(server_id) DO UPDATE SET
                profile_id = excluded.profile_id,
                device_id  = excluded.device_id,
                user_id    = excluded.user_id,
                enabled    = excluded.enabled",
            params![
                profile.server_id,
                profile.profile_id,
                profile.device_id,
                profile.user_id,
                profile.paused as i64,
                profile.enabled as i64,
                chrono::Utc::now().to_rfc3339(),
            ],
        )
        .map_err(map_write)?;
        Ok(())
    }

    pub fn profile(&self, server_id: &str) -> Result<Option<Profile>, AppError> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT server_id, profile_id, device_id, user_id, paused, enabled
             FROM profiles WHERE server_id = ?1",
            params![server_id],
            |row| {
                Ok(Profile {
                    server_id: row.get(0)?,
                    profile_id: row.get(1)?,
                    device_id: row.get(2)?,
                    user_id: row.get(3)?,
                    paused: row.get::<_, i64>(4)? != 0,
                    enabled: row.get::<_, i64>(5)? != 0,
                })
            },
        )
        .optional()
        .map_err(map_read)
    }

    pub fn set_paused(&self, server_id: &str, paused: bool) -> Result<(), AppError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE profiles SET paused = ?2 WHERE server_id = ?1",
            params![server_id, paused as i64],
        )
        .map_err(map_write)?;
        Ok(())
    }

    /// Record that the server has turned backup off for this computer.
    ///
    /// The profile row and its roots are kept. Backup being off is a setting,
    /// not an amnesia: the folders are still stored on the server, and the
    /// only way to offer them back is to remember which ones they were.
    ///
    /// Outstanding work is dropped, because it is no longer going anywhere.
    /// Entries already sent are kept, because the server still has them and
    /// re-enabling must not mean uploading everything a second time.
    pub fn disable_profile(&self, server_id: &str) -> Result<(), AppError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM entries
             WHERE server_id = ?1 AND state IN ('PENDING', 'IN_PROGRESS', 'FAILED')",
            params![server_id],
        )
        .map_err(map_write)?;
        conn.execute(
            "UPDATE roots SET enabled = 0 WHERE server_id = ?1",
            params![server_id],
        )
        .map_err(map_write)?;
        conn.execute(
            "UPDATE profiles SET enabled = 0, paused = 0 WHERE server_id = ?1",
            params![server_id],
        )
        .map_err(map_write)?;
        tracing::info!(server_id, "backup marked off for this server");
        Ok(())
    }

    /// Record that backup is on again. Roots stay as they are — turning backup
    /// back on is not the same as re-protecting every folder.
    pub fn enable_profile(&self, server_id: &str) -> Result<(), AppError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE profiles SET enabled = 1 WHERE server_id = ?1",
            params![server_id],
        )
        .map_err(map_write)?;
        Ok(())
    }

    /// Forget everything for one server. Used when backup is disabled or the
    /// device is revoked. Other servers are untouched.
    pub fn forget_server(&self, server_id: &str) -> Result<(), AppError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM entries WHERE server_id = ?1",
            params![server_id],
        )
        .map_err(map_write)?;
        conn.execute("DELETE FROM roots WHERE server_id = ?1", params![server_id])
            .map_err(map_write)?;
        conn.execute(
            "DELETE FROM profiles WHERE server_id = ?1",
            params![server_id],
        )
        .map_err(map_write)?;
        tracing::info!(server_id, "local backup state cleared for server");
        Ok(())
    }

    // --- Roots -----------------------------------------------------------

    pub fn save_root(&self, server_id: &str, root: &Root) -> Result<(), AppError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO roots (id, server_id, kind, display_name, local_path, enabled)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(server_id, id) DO UPDATE SET
                kind         = excluded.kind,
                display_name = excluded.display_name,
                local_path   = excluded.local_path,
                enabled      = excluded.enabled",
            params![
                root.id,
                server_id,
                root.kind,
                root.display_name,
                root.local_path.to_string_lossy(),
                root.enabled as i64,
            ],
        )
        .map_err(map_write)?;
        Ok(())
    }

    pub fn roots(&self, server_id: &str) -> Result<Vec<Root>, AppError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT id, kind, display_name, local_path, enabled
                 FROM roots WHERE server_id = ?1 ORDER BY display_name",
            )
            .map_err(map_read)?;
        let rows = stmt
            .query_map(params![server_id], |row| {
                Ok(Root {
                    id: row.get(0)?,
                    kind: row.get(1)?,
                    display_name: row.get(2)?,
                    local_path: PathBuf::from(row.get::<_, String>(3)?),
                    enabled: row.get::<_, i64>(4)? != 0,
                })
            })
            .map_err(map_read)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(map_read)
    }

    pub fn set_root_enabled(
        &self,
        server_id: &str,
        root_id: &str,
        enabled: bool,
    ) -> Result<(), AppError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE roots SET enabled = ?3 WHERE server_id = ?1 AND id = ?2",
            params![server_id, root_id, enabled as i64],
        )
        .map_err(map_write)?;
        Ok(())
    }

    // --- Entries ---------------------------------------------------------

    /// Find an entry by its path within a root, case-insensitively.
    ///
    /// This is how a rescan recognises a file it has seen before and reuses its
    /// `clientEntryId` instead of minting a second identity for it.
    pub fn entry_by_path(
        &self,
        server_id: &str,
        root_id: &str,
        relative_path: &str,
    ) -> Result<Option<Entry>, AppError> {
        let key = crate::backup::protocol::path_identity_key(relative_path);
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT client_entry_id, root_id, relative_path, entry_type, size_bytes,
                    modified_ms, state, pending_operation_id
             FROM entries WHERE server_id = ?1 AND root_id = ?2 AND path_key = ?3",
            params![server_id, root_id, key],
            read_entry,
        )
        .optional()
        .map_err(map_read)
    }

    pub fn entry_by_id(
        &self,
        server_id: &str,
        client_entry_id: &str,
    ) -> Result<Option<Entry>, AppError> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT client_entry_id, root_id, relative_path, entry_type, size_bytes,
                    modified_ms, state, pending_operation_id
             FROM entries WHERE server_id = ?1 AND client_entry_id = ?2",
            params![server_id, client_entry_id],
            read_entry,
        )
        .optional()
        .map_err(map_read)
    }

    /// Insert or update one entry.
    pub fn upsert_entry(&self, server_id: &str, entry: &Entry) -> Result<(), AppError> {
        let key = crate::backup::protocol::path_identity_key(&entry.relative_path);
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO entries (
                client_entry_id, server_id, root_id, relative_path, path_key,
                entry_type, size_bytes, modified_ms, state, pending_operation_id, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(server_id, client_entry_id) DO UPDATE SET
                root_id              = excluded.root_id,
                relative_path        = excluded.relative_path,
                path_key             = excluded.path_key,
                entry_type           = excluded.entry_type,
                size_bytes           = excluded.size_bytes,
                modified_ms          = excluded.modified_ms,
                state                = excluded.state,
                pending_operation_id = excluded.pending_operation_id,
                updated_at           = excluded.updated_at",
            params![
                entry.client_entry_id,
                server_id,
                entry.root_id,
                entry.relative_path,
                key,
                entry.entry_type.as_str(),
                entry.size_bytes,
                entry.modified_ms,
                entry.state.as_str(),
                entry.pending_operation_id,
                chrono::Utc::now().to_rfc3339(),
            ],
        )
        .map_err(map_write)?;
        Ok(())
    }

    /// Claim an entry for sending: record the operation id **before** the
    /// request goes out, so a crash mid-flight leaves a retryable record with
    /// the same key rather than an unknown state.
    pub fn begin_operation(
        &self,
        server_id: &str,
        client_entry_id: &str,
        operation_id: &str,
    ) -> Result<(), AppError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE entries SET state = ?3, pending_operation_id = ?4, updated_at = ?5
             WHERE server_id = ?1 AND client_entry_id = ?2",
            params![
                server_id,
                client_entry_id,
                SyncState::InProgress.as_str(),
                operation_id,
                chrono::Utc::now().to_rfc3339(),
            ],
        )
        .map_err(map_write)?;
        Ok(())
    }

    /// Mark an entry acknowledged by the server and clear its pending key.
    pub fn complete_operation(
        &self,
        server_id: &str,
        client_entry_id: &str,
        state: SyncState,
    ) -> Result<(), AppError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE entries SET state = ?3, pending_operation_id = NULL, updated_at = ?4
             WHERE server_id = ?1 AND client_entry_id = ?2",
            params![
                server_id,
                client_entry_id,
                state.as_str(),
                chrono::Utc::now().to_rfc3339(),
            ],
        )
        .map_err(map_write)?;
        Ok(())
    }

    /// Entries still owed to the server, oldest first.
    ///
    /// `IN_PROGRESS` is included deliberately: after a crash those are not
    /// known to have succeeded, and replaying them with their stored
    /// `operationId` is exactly what idempotency is for.
    pub fn outstanding(&self, server_id: &str, limit: usize) -> Result<Vec<Entry>, AppError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT client_entry_id, root_id, relative_path, entry_type, size_bytes,
                        modified_ms, state, pending_operation_id
                 FROM entries
                 WHERE server_id = ?1 AND state IN ('PENDING', 'IN_PROGRESS', 'FAILED')
                 -- Folders first, then shallowest first, then by path.
                 --
                 -- `entry_type DESC` puts FOLDER ahead of FILE. Depth is what
                 -- makes parent-before-child true: a parent is always strictly
                 -- shallower than its child, so ordering by separator count
                 -- puts every ancestor ahead of every descendant without
                 -- building a dependency graph. Ordering by `updated_at` (as
                 -- this did) ordered by when the scan happened to insert a
                 -- row, which let a child be created before its parent.
                 --
                 -- The path tiebreak keeps same-depth entries in a stable,
                 -- deterministic order rather than whatever SQLite returns.
                 ORDER BY entry_type DESC,
                          (length(relative_path) - length(replace(relative_path, '/', ''))) ASC,
                          relative_path ASC
                 LIMIT ?2",
            )
            .map_err(map_read)?;
        let rows = stmt
            .query_map(params![server_id, limit as i64], read_entry)
            .map_err(map_read)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(map_read)
    }

    /// Every live entry under one root, for reconciliation.
    pub fn entries_in_root(&self, server_id: &str, root_id: &str) -> Result<Vec<Entry>, AppError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT client_entry_id, root_id, relative_path, entry_type, size_bytes,
                        modified_ms, state, pending_operation_id
                 FROM entries
                 WHERE server_id = ?1 AND root_id = ?2 AND state != 'TOMBSTONED'",
            )
            .map_err(map_read)?;
        let rows = stmt
            .query_map(params![server_id, root_id], read_entry)
            .map_err(map_read)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(map_read)
    }

    /// Counts for the status surface: (synced, outstanding, failed, bytes synced).
    /// Counts and byte totals for one server: synced, outstanding, failed,
    /// bytes already sent, and bytes still to send.
    ///
    /// The last one is what lets the UI say "4.2 GB to go" rather than only a
    /// file count — a thousand small files and a thousand videos are very
    /// different waits.
    pub fn progress(&self, server_id: &str) -> Result<(i64, i64, i64, i64, i64), AppError> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT
                COALESCE(SUM(state = 'SYNCED'), 0),
                COALESCE(SUM(state IN ('PENDING', 'IN_PROGRESS')), 0),
                COALESCE(SUM(state = 'FAILED'), 0),
                COALESCE(SUM(CASE WHEN state = 'SYNCED' THEN size_bytes ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN state IN ('PENDING', 'IN_PROGRESS', 'FAILED')
                                  THEN size_bytes ELSE 0 END), 0)
             FROM entries WHERE server_id = ?1 AND entry_type = 'FILE'",
            params![server_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .map_err(map_read)
    }

    /// The same counters, for one protected root.
    ///
    /// The Backup Center shows each folder's own size and file count, which is
    /// how a person tells "my Desktop is backed up" from "my Desktop started
    /// backing up and stopped".
    pub fn root_progress(
        &self,
        server_id: &str,
        root_id: &str,
    ) -> Result<(i64, i64, i64, i64), AppError> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT
                COALESCE(SUM(state = 'SYNCED'), 0),
                COALESCE(SUM(state IN ('PENDING', 'IN_PROGRESS')), 0),
                COALESCE(SUM(state = 'FAILED'), 0),
                COALESCE(SUM(CASE WHEN state = 'SYNCED' THEN size_bytes ELSE 0 END), 0)
             FROM entries
             WHERE server_id = ?1 AND root_id = ?2 AND entry_type = 'FILE'",
            params![server_id, root_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .map_err(map_read)
    }

    /// When this server last had a file land successfully.
    pub fn last_synced_at(&self, server_id: &str) -> Result<Option<String>, AppError> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT MAX(updated_at) FROM entries
             WHERE server_id = ?1 AND state = 'SYNCED'",
            params![server_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .map_err(map_read)
    }

    /// Stop backing up one root.
    ///
    /// The root is disabled rather than deleted: files already on the server
    /// stay there, and nothing local is touched.
    ///
    /// Its *queued* entries are dropped, not marked done. Marking them
    /// `SYNCED` was the obvious shortcut and it was wrong: a folder removed
    /// mid-scan then reported a hundred thousand files as backed up when not
    /// one of them had been sent. The counters a person reads have to mean
    /// what they say. Entries that really were sent keep their `SYNCED` rows,
    /// so re-adding the folder later still recognises them and re-uploads
    /// nothing.
    pub fn disable_root(&self, server_id: &str, root_id: &str) -> Result<(), AppError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE roots SET enabled = 0 WHERE server_id = ?1 AND id = ?2",
            params![server_id, root_id],
        )
        .map_err(map_write)?;
        conn.execute(
            "DELETE FROM entries
             WHERE server_id = ?1 AND root_id = ?2
               AND state IN ('PENDING', 'IN_PROGRESS', 'FAILED')",
            params![server_id, root_id],
        )
        .map_err(map_write)?;
        Ok(())
    }
}

fn read_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<Entry> {
    Ok(Entry {
        client_entry_id: row.get(0)?,
        root_id: row.get(1)?,
        relative_path: row.get(2)?,
        entry_type: EntryType::parse(&row.get::<_, String>(3)?),
        size_bytes: row.get(4)?,
        modified_ms: row.get(5)?,
        state: SyncState::parse(&row.get::<_, String>(6)?),
        pending_operation_id: row.get(7)?,
    })
}

fn map_write(err: rusqlite::Error) -> AppError {
    tracing::error!(error = %err, "sync state write failed");
    AppError::internal("Backup state could not be saved.")
}

fn map_read(err: rusqlite::Error) -> AppError {
    tracing::error!(error = %err, "sync state read failed");
    AppError::internal("Backup state could not be read.")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SERVER_A: &str = "3f2504e0-4f89-41d3-9a0c-0305e82c3301";
    const SERVER_B: &str = "9c858901-8a57-4791-81fe-4c455b099bc9";

    fn store() -> (SyncStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = SyncStore::open(dir.path()).unwrap();
        (store, dir)
    }

    fn profile(server_id: &str) -> Profile {
        Profile {
            server_id: server_id.into(),
            profile_id: "profile-1".into(),
            device_id: "device-1".into(),
            user_id: "user-1".into(),
            paused: false,
            enabled: true,
        }
    }

    fn entry(path: &str, id: &str) -> Entry {
        Entry {
            client_entry_id: id.into(),
            root_id: "root-1".into(),
            relative_path: path.into(),
            entry_type: EntryType::File,
            size_bytes: 10,
            modified_ms: 1_700_000_000_000,
            state: SyncState::Pending,
            pending_operation_id: None,
        }
    }

    #[test]
    fn a_profile_round_trips() {
        let (store, _dir) = store();
        store.save_profile(&profile(SERVER_A)).unwrap();
        let loaded = store.profile(SERVER_A).unwrap().unwrap();
        assert_eq!(loaded.profile_id, "profile-1");
        assert!(!loaded.paused);
    }

    #[test]
    fn entry_identity_survives_a_rename() {
        // The whole point of clientEntryId: the path changes, the identity does
        // not, so a rename is a move server-side rather than a re-upload.
        let (store, _dir) = store();
        store.save_profile(&profile(SERVER_A)).unwrap();

        let mut item = entry("ProjectA/readme.md", "uuid-1");
        store.upsert_entry(SERVER_A, &item).unwrap();

        item.relative_path = "ProjectB/readme.md".into();
        store.upsert_entry(SERVER_A, &item).unwrap();

        assert!(store
            .entry_by_path(SERVER_A, "root-1", "ProjectA/readme.md")
            .unwrap()
            .is_none());
        let moved = store
            .entry_by_path(SERVER_A, "root-1", "ProjectB/readme.md")
            .unwrap()
            .unwrap();
        assert_eq!(moved.client_entry_id, "uuid-1");
    }

    #[test]
    fn lookup_is_case_insensitive_like_windows() {
        let (store, _dir) = store();
        store.save_profile(&profile(SERVER_A)).unwrap();
        store
            .upsert_entry(SERVER_A, &entry("Docs/Report.pdf", "uuid-1"))
            .unwrap();

        let found = store
            .entry_by_path(SERVER_A, "root-1", "docs/report.pdf")
            .unwrap();
        assert_eq!(found.unwrap().client_entry_id, "uuid-1");
    }

    #[test]
    fn one_windows_path_cannot_become_two_identities() {
        let (store, _dir) = store();
        store.save_profile(&profile(SERVER_A)).unwrap();
        store
            .upsert_entry(SERVER_A, &entry("Report.pdf", "uuid-1"))
            .unwrap();

        // Same file, different casing, different id: the unique index must
        // refuse it rather than let the server receive two logical files.
        let clash = store.upsert_entry(SERVER_A, &entry("report.pdf", "uuid-2"));
        assert!(clash.is_err(), "case-variant duplicate must be rejected");
    }

    #[test]
    fn servers_are_isolated() {
        let (store, _dir) = store();
        store.save_profile(&profile(SERVER_A)).unwrap();
        store.save_profile(&profile(SERVER_B)).unwrap();
        store
            .upsert_entry(SERVER_A, &entry("a.txt", "uuid-a"))
            .unwrap();
        store
            .upsert_entry(SERVER_B, &entry("a.txt", "uuid-b"))
            .unwrap();

        assert_eq!(
            store
                .entry_by_path(SERVER_A, "root-1", "a.txt")
                .unwrap()
                .unwrap()
                .client_entry_id,
            "uuid-a"
        );
        assert_eq!(
            store
                .entry_by_path(SERVER_B, "root-1", "a.txt")
                .unwrap()
                .unwrap()
                .client_entry_id,
            "uuid-b"
        );
    }

    #[test]
    fn forgetting_one_server_leaves_the_other_intact() {
        let (store, _dir) = store();
        store.save_profile(&profile(SERVER_A)).unwrap();
        store.save_profile(&profile(SERVER_B)).unwrap();
        store
            .upsert_entry(SERVER_A, &entry("a.txt", "uuid-a"))
            .unwrap();
        store
            .upsert_entry(SERVER_B, &entry("b.txt", "uuid-b"))
            .unwrap();

        store.forget_server(SERVER_A).unwrap();

        assert!(store.profile(SERVER_A).unwrap().is_none());
        assert!(store.profile(SERVER_B).unwrap().is_some());
        assert!(store.entry_by_id(SERVER_B, "uuid-b").unwrap().is_some());
    }

    #[test]
    fn an_in_flight_operation_is_replayed_not_lost() {
        // Simulates a crash: the operation was recorded, never completed. It
        // must come back as outstanding, carrying its original key.
        let (store, _dir) = store();
        store.save_profile(&profile(SERVER_A)).unwrap();
        store
            .upsert_entry(SERVER_A, &entry("a.txt", "uuid-a"))
            .unwrap();
        store.begin_operation(SERVER_A, "uuid-a", "op-123").unwrap();

        let outstanding = store.outstanding(SERVER_A, 10).unwrap();
        assert_eq!(outstanding.len(), 1);
        assert_eq!(outstanding[0].state, SyncState::InProgress);
        assert_eq!(
            outstanding[0].pending_operation_id.as_deref(),
            Some("op-123")
        );
    }

    #[test]
    fn completing_clears_the_pending_key() {
        let (store, _dir) = store();
        store.save_profile(&profile(SERVER_A)).unwrap();
        store
            .upsert_entry(SERVER_A, &entry("a.txt", "uuid-a"))
            .unwrap();
        store.begin_operation(SERVER_A, "uuid-a", "op-123").unwrap();
        store
            .complete_operation(SERVER_A, "uuid-a", SyncState::Synced)
            .unwrap();

        let done = store.entry_by_id(SERVER_A, "uuid-a").unwrap().unwrap();
        assert_eq!(done.state, SyncState::Synced);
        assert!(done.pending_operation_id.is_none());
        assert!(store.outstanding(SERVER_A, 10).unwrap().is_empty());
    }

    #[test]
    fn folders_are_sent_before_files() {
        // The server creates parents on demand, but sending folders first keeps
        // the tree correct for empty directories too.
        let (store, _dir) = store();
        store.save_profile(&profile(SERVER_A)).unwrap();
        store
            .upsert_entry(SERVER_A, &entry("z.txt", "uuid-file"))
            .unwrap();
        let mut folder = entry("AAA", "uuid-folder");
        folder.entry_type = EntryType::Folder;
        store.upsert_entry(SERVER_A, &folder).unwrap();

        let outstanding = store.outstanding(SERVER_A, 10).unwrap();
        assert_eq!(outstanding[0].entry_type, EntryType::Folder);
    }

    /// Insert a folder, in whatever order the caller likes.
    fn put_folder(store: &SyncStore, path: &str, id: &str) {
        let mut e = entry(path, id);
        e.entry_type = EntryType::Folder;
        store.upsert_entry(SERVER_A, &e).unwrap();
    }

    #[test]
    fn a_parent_folder_is_always_queued_before_its_child() {
        // The live bug: `WebProject/public` was created before `WebProject`,
        // so the parent's own create came back ALREADY_EXISTS. Inserted here
        // deepest-first so insertion order cannot be what saves it.
        let (store, _dir) = store();
        store.save_profile(&profile(SERVER_A)).unwrap();
        put_folder(&store, "WebProject/public", "deep");
        put_folder(&store, "WebProject", "shallow");

        let queued: Vec<String> = store
            .outstanding(SERVER_A, 10)
            .unwrap()
            .into_iter()
            .map(|e| e.relative_path)
            .collect();

        let parent = queued.iter().position(|p| p == "WebProject").unwrap();
        let child = queued
            .iter()
            .position(|p| p == "WebProject/public")
            .unwrap();
        assert!(parent < child, "parent must be queued first: {queued:?}");
    }

    #[test]
    fn deeply_nested_folders_come_out_shallowest_first() {
        let (store, _dir) = store();
        store.save_profile(&profile(SERVER_A)).unwrap();
        for (index, path) in ["a/b/c/d", "a", "a/b/c", "a/b"].iter().enumerate() {
            put_folder(&store, path, &format!("id-{index}"));
        }

        let queued: Vec<String> = store
            .outstanding(SERVER_A, 10)
            .unwrap()
            .into_iter()
            .map(|e| e.relative_path)
            .collect();

        assert_eq!(queued, vec!["a", "a/b", "a/b/c", "a/b/c/d"]);
    }

    #[test]
    fn folders_at_the_same_depth_have_a_stable_order() {
        // Deterministic rather than "whatever SQLite felt like", so a rerun
        // reproduces a failure instead of hiding it.
        let (store, _dir) = store();
        store.save_profile(&profile(SERVER_A)).unwrap();
        for (index, path) in ["x/zebra", "x/apple", "x/mango"].iter().enumerate() {
            put_folder(&store, path, &format!("same-{index}"));
        }

        let first: Vec<String> = store
            .outstanding(SERVER_A, 10)
            .unwrap()
            .into_iter()
            .map(|e| e.relative_path)
            .collect();
        let again: Vec<String> = store
            .outstanding(SERVER_A, 10)
            .unwrap()
            .into_iter()
            .map(|e| e.relative_path)
            .collect();

        assert_eq!(first, again);
        assert_eq!(first, vec!["x/apple", "x/mango", "x/zebra"]);
    }

    #[test]
    fn no_file_is_queued_before_a_folder_it_could_need() {
        // Files may be uploaded concurrently, so every folder has to be ahead
        // of every file — not merely ahead of its own parent.
        let (store, _dir) = store();
        store.save_profile(&profile(SERVER_A)).unwrap();
        store
            .upsert_entry(SERVER_A, &entry("WebProject/public/logo.png", "f1"))
            .unwrap();
        store
            .upsert_entry(SERVER_A, &entry("photo.jpg", "f2"))
            .unwrap();
        put_folder(&store, "WebProject/public", "d2");
        put_folder(&store, "WebProject", "d1");

        let queued = store.outstanding(SERVER_A, 10).unwrap();
        let last_folder = queued
            .iter()
            .rposition(|e| e.entry_type == EntryType::Folder)
            .unwrap();
        let first_file = queued
            .iter()
            .position(|e| e.entry_type == EntryType::File)
            .unwrap();

        assert!(
            last_folder < first_file,
            "every folder must precede every file"
        );
        assert_eq!(queued[0].relative_path, "WebProject");
        assert_eq!(queued[1].relative_path, "WebProject/public");
    }

    #[test]
    fn ordering_survives_a_retry_without_losing_identity() {
        // Ordering is the fix; idempotency is still the net. A folder that
        // failed and is retried keeps its client id and its place.
        let (store, _dir) = store();
        store.save_profile(&profile(SERVER_A)).unwrap();
        put_folder(&store, "WebProject", "keep-me");
        put_folder(&store, "WebProject/public", "child");

        let queued = store.outstanding(SERVER_A, 10).unwrap();
        assert_eq!(queued[0].client_entry_id, "keep-me");
        assert_eq!(queued[0].relative_path, "WebProject");
    }

    /// A root to remove, with one file genuinely sent and one still queued.
    fn root_with_a_queue(store: &SyncStore) {
        store.save_profile(&profile(SERVER_A)).unwrap();
        store
            .save_root(
                SERVER_A,
                &Root {
                    id: "root-1".into(),
                    kind: "PICTURES".into(),
                    display_name: "Pictures".into(),
                    local_path: std::path::PathBuf::from(r"C:\Users\x\Pictures"),
                    enabled: true,
                },
            )
            .unwrap();

        let mut sent = entry("already-there.jpg", "sent");
        sent.state = SyncState::Synced;
        store.upsert_entry(SERVER_A, &sent).unwrap();
        store
            .upsert_entry(SERVER_A, &entry("queued.jpg", "queued"))
            .unwrap();
    }

    #[test]
    fn removing_a_folder_does_not_claim_its_queue_was_backed_up() {
        // The bug this pins: removing a folder marked every queued entry
        // SYNCED, so a folder removed mid-scan reported a hundred thousand
        // files as backed up when not one had been sent. The counters a
        // person reads have to mean what they say.
        let (store, _dir) = store();
        root_with_a_queue(&store);

        store.disable_root(SERVER_A, "root-1").unwrap();

        let (synced, outstanding, failed, _, _) = store.progress(SERVER_A).unwrap();
        assert_eq!(synced, 1, "only genuinely sent files may count as synced");
        assert_eq!(outstanding, 0, "the queue must not keep being sent");
        assert_eq!(failed, 0);
    }

    #[test]
    fn a_removed_folder_stops_being_offered_to_the_engine() {
        let (store, _dir) = store();
        root_with_a_queue(&store);

        store.disable_root(SERVER_A, "root-1").unwrap();

        assert!(
            store.outstanding(SERVER_A, 10).unwrap().is_empty(),
            "nothing from a removed folder may still be queued"
        );
        let roots = store.roots(SERVER_A).unwrap();
        assert_eq!(roots.len(), 1);
        assert!(!roots[0].enabled, "the root is kept, but disabled");
    }

    #[test]
    fn the_database_holds_no_secrets() {
        // A guard against someone later "conveniently" caching a credential
        // here: the schema simply has nowhere to put one.
        let (store, dir) = store();
        store.save_profile(&profile(SERVER_A)).unwrap();
        let conn = store.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT name FROM pragma_table_info('profiles')")
            .unwrap();
        let columns: Vec<String> = stmt
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(|c| c.unwrap())
            .collect();
        drop(stmt);
        drop(conn);
        for banned in ["credential", "secret", "token", "password", "cookie"] {
            assert!(
                !columns.iter().any(|c| c.contains(banned)),
                "profiles must not have a {banned} column"
            );
        }
        drop(dir);
    }
    // --- Backup being turned off ----------------------------------------
    //
    // The distinction these cover: turning backup off is a *setting*, and
    // forgetting the server is not. The version this replaces deleted the
    // profile on stop, so a computer that had been switched off was
    // indistinguishable from one that had never been set up — the only way
    // back was first-run setup, which built a second tree on the server
    // beside the files that were already there.

    fn root(id: &str, name: &str) -> Root {
        Root {
            id: id.into(),
            kind: "CUSTOM".into(),
            display_name: name.into(),
            local_path: PathBuf::from(format!(r"D:\Profiles\TestUser\{name}")),
            enabled: true,
        }
    }

    #[test]
    fn turning_backup_off_keeps_the_profile() {
        let (store, _dir) = store();
        store.save_profile(&profile(SERVER_A)).unwrap();

        store.disable_profile(SERVER_A).unwrap();

        let loaded = store.profile(SERVER_A).unwrap().expect("profile kept");
        assert_eq!(loaded.profile_id, "profile-1");
        assert!(!loaded.enabled, "backup must read as off");
    }

    #[test]
    fn turning_backup_off_keeps_the_folders_it_protected() {
        // They are still stored on the server, and remembering which ones
        // they were is the whole of the offer to turn backup back on.
        let (store, _dir) = store();
        store.save_profile(&profile(SERVER_A)).unwrap();
        store
            .save_root(SERVER_A, &root("root-1", "TestBackup"))
            .unwrap();
        store.save_root(SERVER_A, &root("root-2", "Notes")).unwrap();

        store.disable_profile(SERVER_A).unwrap();

        let roots = store.roots(SERVER_A).unwrap();
        assert_eq!(
            roots.len(),
            2,
            "folders must survive backup being turned off"
        );
        assert!(
            roots.iter().all(|r| !r.enabled),
            "but none of them may still be protected"
        );
        assert!(
            roots.iter().any(|r| r.local_path.ends_with("TestBackup")),
            "the local path has to survive too, or the folder cannot be resumed"
        );
    }

    #[test]
    fn turning_backup_off_drops_outstanding_work_but_keeps_what_was_sent() {
        // Queued work is not going anywhere once the grant is revoked, so
        // keeping it would only overstate what is protected. What was already
        // sent is a different matter: the server still has it, and re-enabling
        // must not mean uploading everything a second time.
        let (store, _dir) = store();
        store.save_profile(&profile(SERVER_A)).unwrap();
        store
            .save_root(SERVER_A, &root("root-1", "TestBackup"))
            .unwrap();

        let mut sent = entry("sent.txt", "entry-sent");
        sent.state = SyncState::Synced;
        store.upsert_entry(SERVER_A, &sent).unwrap();
        store
            .upsert_entry(SERVER_A, &entry("queued.txt", "entry-queued"))
            .unwrap();

        store.disable_profile(SERVER_A).unwrap();

        assert!(
            store
                .entry_by_path(SERVER_A, "root-1", "sent.txt")
                .unwrap()
                .is_some(),
            "a file already on the server must not be forgotten"
        );
        assert!(
            store
                .entry_by_path(SERVER_A, "root-1", "queued.txt")
                .unwrap()
                .is_none(),
            "queued work must not survive the grant that was going to send it"
        );
    }

    #[test]
    fn a_computer_whose_backup_is_off_survives_a_restart() {
        // Reopened from disk, because this is exactly the state a launch has
        // to read correctly: the profile is there but backup is off, and
        // resuming would be wrong.
        let dir = tempfile::tempdir().unwrap();
        {
            let store = SyncStore::open(dir.path()).unwrap();
            store.save_profile(&profile(SERVER_A)).unwrap();
            store
                .save_root(SERVER_A, &root("root-1", "TestBackup"))
                .unwrap();
            store.disable_profile(SERVER_A).unwrap();
        }
        let store = SyncStore::open(dir.path()).unwrap();
        let loaded = store.profile(SERVER_A).unwrap().expect("profile kept");
        assert!(!loaded.enabled);
        assert_eq!(store.roots(SERVER_A).unwrap().len(), 1);
    }

    #[test]
    fn turning_backup_back_on_does_not_reprotect_every_folder() {
        // Deliberate. Someone who switched backup off because one enormous
        // folder was filling the server must not have it start again on its
        // own; each folder is resumed by hand.
        let (store, _dir) = store();
        store.save_profile(&profile(SERVER_A)).unwrap();
        store
            .save_root(SERVER_A, &root("root-1", "TestBackup"))
            .unwrap();
        store.disable_profile(SERVER_A).unwrap();

        store.enable_profile(SERVER_A).unwrap();

        assert!(store.profile(SERVER_A).unwrap().unwrap().enabled);
        assert!(
            store.roots(SERVER_A).unwrap().iter().all(|r| !r.enabled),
            "turning backup on is not the same as protecting every folder again"
        );
    }

    #[test]
    fn resuming_a_folder_keeps_the_root_it_always_was() {
        // The identity check behind "no duplicate hierarchy": the id and the
        // local path are unchanged, so the server sees the same SyncRoot and
        // the files already under it stay where they are.
        let (store, _dir) = store();
        store.save_profile(&profile(SERVER_A)).unwrap();
        store
            .save_root(SERVER_A, &root("root-1", "TestBackup"))
            .unwrap();
        let before = store.roots(SERVER_A).unwrap()[0].clone();

        store.disable_profile(SERVER_A).unwrap();
        store.enable_profile(SERVER_A).unwrap();
        store.set_root_enabled(SERVER_A, "root-1", true).unwrap();

        let after = store.roots(SERVER_A).unwrap();
        assert_eq!(after.len(), 1, "resuming must not add a second root");
        assert_eq!(after[0].id, before.id);
        assert_eq!(after[0].local_path, before.local_path);
        assert!(after[0].enabled);
    }

    #[test]
    fn one_servers_backup_being_off_leaves_another_alone() {
        let (store, _dir) = store();
        store.save_profile(&profile(SERVER_A)).unwrap();
        store.save_profile(&profile(SERVER_B)).unwrap();
        store
            .save_root(SERVER_B, &root("root-b", "TestBackup"))
            .unwrap();

        store.disable_profile(SERVER_A).unwrap();

        assert!(store.profile(SERVER_B).unwrap().unwrap().enabled);
        assert!(store.roots(SERVER_B).unwrap()[0].enabled);
    }

    #[test]
    fn revocation_still_erases_everything() {
        // The other lifecycle, and it must stay different: an unpaired device
        // has nothing to offer back, so nothing is kept.
        let (store, _dir) = store();
        store.save_profile(&profile(SERVER_A)).unwrap();
        store
            .save_root(SERVER_A, &root("root-1", "TestBackup"))
            .unwrap();

        store.forget_server(SERVER_A).unwrap();

        assert!(store.profile(SERVER_A).unwrap().is_none());
        assert!(store.roots(SERVER_A).unwrap().is_empty());
    }

    #[test]
    fn a_folder_that_was_never_uploaded_reports_nothing_stored() {
        // What the "not currently protected" list is built from. A folder can
        // be switched off before a single file reaches the server — that is
        // exactly what happened to a Pictures root here once — and reporting
        // it as stored would overstate what is protected in the one place
        // somebody goes to check.
        let (store, _dir) = store();
        store.save_profile(&profile(SERVER_A)).unwrap();
        store
            .save_root(SERVER_A, &root("root-1", "NeverSent"))
            .unwrap();
        store
            .upsert_entry(SERVER_A, &entry("a.txt", "entry-a"))
            .unwrap();

        store.disable_profile(SERVER_A).unwrap();

        let (files, pending, failed, bytes) = store.root_progress(SERVER_A, "root-1").unwrap();
        assert_eq!((files, pending, failed, bytes), (0, 0, 0, 0));
    }

    #[test]
    fn a_folder_that_was_uploaded_still_reports_what_it_sent() {
        // The other half: switching a folder off must not erase the record of
        // what the server already holds, or resuming it would look like
        // starting from nothing.
        let (store, _dir) = store();
        store.save_profile(&profile(SERVER_A)).unwrap();
        store
            .save_root(SERVER_A, &root("root-1", "TestBackup"))
            .unwrap();
        let mut sent = entry("sent.txt", "entry-sent");
        sent.state = SyncState::Synced;
        store.upsert_entry(SERVER_A, &sent).unwrap();

        store.disable_profile(SERVER_A).unwrap();

        let (files, _pending, _failed, bytes) = store.root_progress(SERVER_A, "root-1").unwrap();
        assert_eq!(files, 1);
        assert_eq!(bytes, 10);
    }

    #[test]
    fn a_database_from_before_this_column_still_opens() {
        // Version 1 wrote `profiles` without `enabled`. An upgrade that could
        // not read an existing database would lose someone's whole queue on
        // first launch after an update, so the column is added in place and
        // the computers already backing up keep reading as enabled.
        let dir = tempfile::tempdir().unwrap();
        {
            let conn = Connection::open(dir.path().join("backup-state.db")).unwrap();
            conn.execute_batch(
                "CREATE TABLE profiles (
                     server_id   TEXT PRIMARY KEY,
                     profile_id  TEXT NOT NULL,
                     device_id   TEXT NOT NULL,
                     user_id     TEXT NOT NULL,
                     paused      INTEGER NOT NULL DEFAULT 0,
                     created_at  TEXT NOT NULL
                 );
                 INSERT INTO profiles VALUES
                     ('server-old', 'profile-old', 'device-old', 'user-old', 0, '2026-01-01T00:00:00Z');
                 PRAGMA user_version = 1;",
            )
            .unwrap();
        }

        let store = SyncStore::open(dir.path()).unwrap();
        let loaded = store
            .profile("server-old")
            .unwrap()
            .expect("profile survived");
        assert_eq!(loaded.profile_id, "profile-old");
        assert!(
            loaded.enabled,
            "a computer that was backing up before the upgrade is still backing up"
        );
    }
}
