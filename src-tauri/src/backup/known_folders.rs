//! Resolving the Windows known folders a person can protect.
//!
//! `C:\Users\<name>\Desktop` is a guess, not an answer. Desktop and Documents
//! are routinely redirected — to OneDrive, to a D: drive, to a network share by
//! group policy — and the user profile is not always on C:. So every path here
//! comes from `SHGetKnownFolderPath`, which is what Windows itself uses.
//!
//! Only the resolved path is used locally. What the server is told is an opaque
//! identifier and a display name; the protocol is explicit that V1 does not
//! persist absolute personal paths.

use std::path::PathBuf;

use crate::backup::protocol::SyncRootKind;

/// A known folder that exists on this machine and can be protected.
#[derive(Debug, Clone)]
pub struct KnownFolder {
    pub kind: SyncRootKind,
    /// Resolved by Windows. Stays local.
    pub path: PathBuf,
}

#[cfg(windows)]
mod imp {
    use super::*;

    use windows::core::GUID;
    use windows::Win32::UI::Shell::{
        FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_Music,
        FOLDERID_Pictures, FOLDERID_Videos, SHGetKnownFolderPath, KF_FLAG_DEFAULT,
    };

    fn folder_id(kind: SyncRootKind) -> Option<GUID> {
        Some(match kind {
            SyncRootKind::Desktop => FOLDERID_Desktop,
            SyncRootKind::Documents => FOLDERID_Documents,
            SyncRootKind::Pictures => FOLDERID_Pictures,
            SyncRootKind::Videos => FOLDERID_Videos,
            SyncRootKind::Music => FOLDERID_Music,
            SyncRootKind::Downloads => FOLDERID_Downloads,
            // Not a known folder; a custom root carries its own path.
            SyncRootKind::Custom => return None,
        })
    }

    /// Ask Windows where a known folder actually is.
    ///
    /// `KF_FLAG_DEFAULT` returns the current location, following redirection,
    /// which is exactly what a OneDrive-backed Documents needs.
    pub fn resolve(kind: SyncRootKind) -> Option<PathBuf> {
        let id = folder_id(kind)?;
        // SAFETY: `id` is a static FOLDERID, and the returned PWSTR is freed
        // below with the allocator the API documents (CoTaskMemFree).
        unsafe {
            let raw = SHGetKnownFolderPath(&id, KF_FLAG_DEFAULT, None).ok()?;
            let path = raw.to_string().ok().map(PathBuf::from);
            windows::Win32::System::Com::CoTaskMemFree(Some(raw.0 as *const _));
            path
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;

    pub fn resolve(_kind: SyncRootKind) -> Option<PathBuf> {
        None
    }
}

/// Resolve one known folder, if this machine has it and it is a real directory.
///
/// A path that Windows reports but that does not exist (a disconnected
/// redirection target, say) is treated as absent rather than offered and then
/// failing during the scan.
pub fn resolve(kind: SyncRootKind) -> Option<KnownFolder> {
    let path = imp::resolve(kind)?;
    if !path.is_dir() {
        tracing::info!(
            kind = kind.as_str(),
            "known folder resolved to a path that is not a directory; skipping"
        );
        return None;
    }
    Some(KnownFolder { kind, path })
}

/// Every protectable known folder present on this machine, in display order.
pub fn resolve_all() -> Vec<KnownFolder> {
    SyncRootKind::known_folders()
        .into_iter()
        .filter_map(resolve)
        .collect()
}

/// The opaque `sourcePathIdentifier` sent to the server for a root.
///
/// The protocol requires an identifier that is **not** a Windows path, so the
/// local path never leaves this machine. It has to be stable across restarts
/// (the same folder must map to the same root) without being reversible into a
/// path, so it is derived by hashing the lowercased path with the server's id
/// as a salt — the same folder on two different servers gets two different
/// identifiers, which keeps servers from correlating a machine between them.
pub fn source_path_identifier(
    server_id: &str,
    kind: SyncRootKind,
    path: &std::path::Path,
) -> String {
    source_path_identifier_for_kind(server_id, kind.as_str(), path)
}

/// The same identifier, for a kind that has already been stored as text.
///
/// The persisted root table keeps `kind` as the wire string, so recovering the
/// identifier for a saved root does not need the enum back — and must not,
/// because a kind the server sent that this build does not know would
/// otherwise be silently rewritten into something else and hash differently.
pub fn source_path_identifier_for_kind(
    server_id: &str,
    kind: &str,
    path: &std::path::Path,
) -> String {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(server_id.as_bytes());
    hasher.update([0]);
    hasher.update(kind.as_bytes());
    hasher.update([0]);
    // Windows paths are case-insensitive, so the same folder reached by a
    // differently-cased path must hash the same.
    hasher.update(path.to_string_lossy().to_lowercase().as_bytes());
    let digest = hasher.finalize();

    // Hex of the first 16 bytes: comfortably unique, and well inside the
    // server's 128-character limit.
    digest[..16].iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    const SERVER_A: &str = "3f2504e0-4f89-41d3-9a0c-0305e82c3301";
    const SERVER_B: &str = "9c858901-8a57-4791-81fe-4c455b099bc9";

    #[test]
    fn identifier_is_stable_for_the_same_folder() {
        let path = Path::new(r"D:\Profiles\TestUser\Desktop");
        let first = source_path_identifier(SERVER_A, SyncRootKind::Desktop, path);
        let second = source_path_identifier(SERVER_A, SyncRootKind::Desktop, path);
        assert_eq!(first, second);
    }

    #[test]
    fn identifier_ignores_windows_path_casing() {
        // The same folder, spelled two ways Windows treats as identical. If
        // these hashed differently, protecting Desktop twice under different
        // casing would create two roots and upload the tree twice.
        let lower = Path::new(r"c:\users\testuser\desktop");
        let upper = Path::new(r"C:\Users\TestUser\Desktop");
        assert_eq!(
            source_path_identifier(SERVER_A, SyncRootKind::Desktop, lower),
            source_path_identifier(SERVER_A, SyncRootKind::Desktop, upper),
        );
    }

    #[test]
    fn identifier_differs_per_server() {
        // One machine must not be correlatable across two Arciin servers.
        let path = Path::new(r"C:\Users\TestUser\Desktop");
        assert_ne!(
            source_path_identifier(SERVER_A, SyncRootKind::Desktop, path),
            source_path_identifier(SERVER_B, SyncRootKind::Desktop, path),
        );
    }

    #[test]
    fn identifier_differs_per_kind() {
        let path = Path::new(r"C:\Users\TestUser\Stuff");
        assert_ne!(
            source_path_identifier(SERVER_A, SyncRootKind::Desktop, path),
            source_path_identifier(SERVER_A, SyncRootKind::Documents, path),
        );
    }

    #[test]
    fn identifier_never_contains_the_path() {
        let path = Path::new(r"C:\Users\TestUser\Desktop");
        let id = source_path_identifier(SERVER_A, SyncRootKind::Desktop, path);
        let lowered = id.to_lowercase();
        for leak in ["users", "testuser", "desktop", "c:"] {
            assert!(!lowered.contains(leak), "identifier leaked {leak}");
        }
    }

    #[test]
    fn identifier_fits_the_server_limit() {
        let id = source_path_identifier(SERVER_A, SyncRootKind::Desktop, Path::new("x"));
        assert_eq!(id.len(), 32);
        assert!(id.len() <= crate::backup::protocol::SOURCE_PATH_ID_MAX);
    }
}
