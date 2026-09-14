//! The Arciin computer-backup contract.
//!
//! Source of truth, in this order:
//!
//! 1. `arciin-main/docs/DESKTOP-SYNC-PROTOCOL.md`
//! 2. `arciin-main/apps/api/src/modules/backup/routes.ts` — the deployed
//!    implementation, which **wins** where it differs from the document
//! 3. `arciin-main/packages/config/src/computer-backup.ts` — limits
//!
//! Reference server: `main` @ `85553f6af408bb524b470b1d35f1d66a1e07ab48`.
//!
//! Two places the implementation is stricter than the document, both verified
//! by reading the route handlers:
//!
//! - `move` and `tombstone` require **`syncRootId` in the body**. The
//!   document's examples omit it; the Zod schemas require it.
//! - There are routes the document does not list: `GET /backup/me`,
//!   `POST /backup/roots`, `POST /backup/folders`. Roots and folders are
//!   created explicitly rather than only implied by a file upload.
//!
//! Nothing here is invented.

use serde::{Deserialize, Serialize};

/// `ARCIIN_COMPUTER_BACKUP_PROTOCOL_VERSION`. Independent of pairing.
pub const BACKUP_PROTOCOL_VERSION: u32 = 1;

/// `ARCIIN_SYNC_AUTHORIZATION_SCHEME`. Used as `Authorization: ArciinSync <credential>`.
pub const SYNC_AUTH_SCHEME: &str = "ArciinSync";

/// Every issued backup credential carries this prefix.
pub const CREDENTIAL_PREFIX: &str = "arcsync_";

// --- Routes -------------------------------------------------------------
//
// All are under the web origin's `/api` proxy, same as pairing.

/// Create or update a backup profile. **User session**, not ArciinSync.
pub const PROFILES_PATH: &str = "/api/backup/profiles";
/// Validate the stored sync credential and read current profile state.
pub const ME_PATH: &str = "/api/backup/me";
/// Report sync health. Throttled server-side to roughly once a minute.
pub const HEARTBEAT_PATH: &str = "/api/backup/heartbeat";
/// Create or update a protected root.
pub const ROOTS_PATH: &str = "/api/backup/roots";
/// Create a folder entry, so an empty directory still exists server-side.
pub const FOLDERS_PATH: &str = "/api/backup/folders";
/// Upload or replace a file.
pub const FILES_PATH: &str = "/api/backup/files";

/// `POST /api/backup/entries/:clientEntryId/move`
pub fn move_path(client_entry_id: &str) -> String {
    format!("/api/backup/entries/{client_entry_id}/move")
}

/// `POST /api/backup/entries/:clientEntryId/tombstone`
pub fn tombstone_path(client_entry_id: &str) -> String {
    format!("/api/backup/entries/{client_entry_id}/tombstone")
}

/// `POST /api/backup/profiles/:id/disable`
pub fn disable_path(profile_id: &str) -> String {
    format!("/api/backup/profiles/{profile_id}/disable")
}

// --- Server limits ------------------------------------------------------
//
// From `packages/config/src/computer-backup.ts`. Enforced client-side too, so
// an over-long path is reported clearly instead of being rejected mid-upload.

pub const RELATIVE_PATH_MAX: usize = 1024;
pub const PATH_SEGMENT_MAX: usize = 255;
pub const PATH_DEPTH_MAX: usize = 32;
pub const DISPLAY_NAME_MAX: usize = 120;
pub const CLIENT_ENTRY_ID_MAX: usize = 128;
pub const SOURCE_PATH_ID_MAX: usize = 128;
pub const OPERATION_ID_MAX: usize = 128;

/// `SYNC_ROOT_KINDS`. `CUSTOM` exists but V1 only offers known folders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum SyncRootKind {
    Desktop,
    Documents,
    Pictures,
    Videos,
    Music,
    Downloads,
    Custom,
}

impl SyncRootKind {
    /// The wire value the server's `parseSyncRootKind` expects.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Desktop => "DESKTOP",
            Self::Documents => "DOCUMENTS",
            Self::Pictures => "PICTURES",
            Self::Videos => "VIDEOS",
            Self::Music => "MUSIC",
            Self::Downloads => "DOWNLOADS",
            Self::Custom => "CUSTOM",
        }
    }

    /// What the user sees, and the server's `displayName`.
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Desktop => "Desktop",
            Self::Documents => "Documents",
            Self::Pictures => "Pictures",
            Self::Videos => "Videos",
            Self::Music => "Music",
            Self::Downloads => "Downloads",
            Self::Custom => "Folder",
        }
    }

    /// The six known folders offered during onboarding, in display order.
    pub fn known_folders() -> [SyncRootKind; 6] {
        [
            Self::Desktop,
            Self::Documents,
            Self::Pictures,
            Self::Videos,
            Self::Music,
            Self::Downloads,
        ]
    }

    /// Suggested initial selection.
    ///
    /// Desktop, Documents and Pictures are the folders people actually mean by
    /// "my stuff". Videos, Music and Downloads are commonly enormous and often
    /// re-downloadable, so they start off and stay the user's explicit choice.
    pub fn default_selected(self) -> bool {
        matches!(self, Self::Desktop | Self::Documents | Self::Pictures)
    }
}

/// `health` on `POST /api/backup/heartbeat`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum BackupHealth {
    UpToDate,
    Syncing,
    Paused,
    Offline,
    Error,
}

impl BackupHealth {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UpToDate => "UP_TO_DATE",
            Self::Syncing => "SYNCING",
            Self::Paused => "PAUSED",
            Self::Offline => "OFFLINE",
            Self::Error => "ERROR",
        }
    }
}

/// `capabilities.computerBackup` in the discovery manifest.
///
/// Absent on a server that predates backup, which must keep working.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ComputerBackupCapability {
    #[serde(default)]
    pub supported: bool,
    #[serde(default)]
    pub protocol_version: u32,
}

impl ComputerBackupCapability {
    /// Whether this build can actually talk backup to that server.
    pub fn usable(&self) -> bool {
        self.supported && self.protocol_version == BACKUP_PROTOCOL_VERSION
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ServerCapabilities {
    #[serde(default)]
    pub computer_backup: Option<ComputerBackupCapability>,
}

/// `BackupRootPublic`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupRoot {
    pub id: String,
    pub kind: String,
    pub display_name: String,
    pub source_path_identifier: String,
    #[serde(default)]
    pub status: Option<String>,
}

/// `BackupProfilePublic`, trimmed to the fields this client acts on.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupProfile {
    pub id: String,
    pub device_id: String,
    pub user_id: String,
    pub status: String,
    #[serde(default)]
    pub roots: Vec<BackupRoot>,
}

/// `BackupEntryPublic`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupEntry {
    pub id: String,
    pub sync_root_id: String,
    pub client_entry_id: String,
    pub relative_path: String,
    pub entry_type: String,
    #[serde(default)]
    pub sync_state: Option<String>,
}

/// Friendly copy for the backup error codes in section 17 of the protocol.
///
/// Returns `None` for codes this module does not own, so the caller can fall
/// through to the shared pairing/transport mapping.
pub fn friendly_backup_error(code: &str) -> Option<(&'static str, bool)> {
    let entry = match code {
        "BACKUP_NOT_SUPPORTED" => ("This Arciin server doesn't support computer backup.", false),
        "BACKUP_UNAUTHORIZED" => ("Sign in to Arciin before setting up backup.", false),
        "BACKUP_CREDENTIAL_INVALID" => (
            "This computer's backup authorization is no longer valid. Set backup up again.",
            false,
        ),
        "BACKUP_DEVICE_UNPAIRED" => (
            "This computer is no longer paired with this Arciin server.",
            false,
        ),
        // `BACKUP_DISABLED`, `BACKUP_FORBIDDEN` and `BACKUP_READ_ONLY` are
        // deliberately absent. They are lifecycle answers — backup is off,
        // this account may not manage it, the server is not taking uploads —
        // and their copy lives with the rest of the lifecycle codes in
        // `crate::error`, which says how to get out of each. Owning them here
        // too meant the same situation read two different ways depending on
        // whether the client or the server noticed it, and the version here
        // was the dead-end one: it said backup was off without saying that
        // turning it back on is a thing you can do.
        "PATH_TRAVERSAL" => ("That file's location couldn't be sent safely.", false),
        "PATH_INVALID" => (
            "That file's name or location isn't valid for Arciin.",
            false,
        ),
        "PATH_TOO_LONG" => ("That file's path is too long for Arciin to store.", false),
        "BACKUP_IDEMPOTENCY_CONFLICT" => ("That change was already recorded differently.", false),
        _ => return None,
    };
    Some(entry)
}

/// Normalize a Windows-relative path into the logical form the server expects.
///
/// The server rejects `..`, absolute paths, drive letters and UNC paths, and
/// normalizes separators itself — but sending a bad path only to have it
/// refused mid-upload wastes the transfer, so the same rules are applied here.
///
/// Returns `None` when the path cannot be represented safely.
pub fn normalize_relative_path(raw: &str) -> Option<String> {
    // Reject anything that is not clearly relative before looking at segments.
    if raw.contains('\0') {
        return None;
    }
    let unified = raw.replace('\\', "/");
    if unified.starts_with('/') || unified.starts_with("//") {
        return None;
    }
    // A drive letter, e.g. `C:/...` or a bare `C:`.
    let bytes = unified.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        return None;
    }

    let mut segments: Vec<&str> = Vec::new();
    for segment in unified.split('/') {
        match segment {
            // Collapse empty and `.` segments rather than failing: they carry
            // no meaning and Windows produces them routinely.
            "" | "." => continue,
            // `..` is never resolved locally — it would let a crafted path
            // climb out of the protected root.
            ".." => return None,
            other => {
                if other.len() > PATH_SEGMENT_MAX {
                    return None;
                }
                // Windows trailing dots/spaces are not addressable and the
                // server rejects them.
                if other.ends_with('.') || other.ends_with(' ') {
                    return None;
                }
                segments.push(other);
            }
        }
    }

    if segments.is_empty() || segments.len() > PATH_DEPTH_MAX {
        return None;
    }
    let joined = segments.join("/");
    if joined.len() > RELATIVE_PATH_MAX {
        return None;
    }
    Some(joined)
}

/// Windows compares paths case-insensitively, so `Report.pdf` and `report.pdf`
/// in one root are the same entry. This is the key used for local identity
/// lookups — never for what is sent to the server.
pub fn path_identity_key(relative_path: &str) -> String {
    relative_path.to_lowercase()
}
