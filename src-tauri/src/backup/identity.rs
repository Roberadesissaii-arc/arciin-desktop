//! Telling that two paths are the same file.
//!
//! # Why a heuristic will not do
//!
//! When a file moves, the watcher usually reports it as a pair — gone from
//! here, arrived there — and the pair carries the identity. Usually. Across
//! directories Windows sometimes reports two unrelated-looking events, and
//! while the app is closed it reports nothing at all; the next scan simply
//! finds one path missing and another present.
//!
//! The tempting shortcut is to match them on name, size and modification time.
//! It is wrong in a way that matters: a folder of exported images, a build
//! output tree, a set of empty placeholder files — all routinely contain many
//! files agreeing on all three. Matching the wrong pair does not merely waste
//! bandwidth; it tells the server to move somebody's file on top of another
//! one, and the losing file is gone.
//!
//! So this asks Windows instead. NTFS gives every file a number that is unique
//! on its volume and survives being renamed or moved within it, which is
//! exactly the question being asked. There is no guessing involved.
//!
//! # Where it may go
//!
//! Nowhere. It is a local fact about this machine's filesystem, kept in the
//! local database to recognise a file that has moved. It is never sent, never
//! logged, and never reaches the UI — a file index plus a volume serial says
//! something about the user's disk layout that the server has no business
//! knowing and no use for.

/// A file's identity on this machine: which volume, and which file on it.
///
/// Stored as text because that is all the database and the comparison need,
/// and because the numeric halves have no meaning apart from each other.
pub type FileIdentity = String;

#[cfg(windows)]
mod imp {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use windows::core::PCWSTR;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_NORMAL,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };

    pub fn identify(path: &Path) -> Option<super::FileIdentity> {
        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        // SAFETY: `wide` is a NUL-terminated UTF-16 path that outlives the
        // call, and the handle is closed on every path out below.
        unsafe {
            let handle = CreateFileW(
                PCWSTR(wide.as_ptr()),
                // No access rights at all. Identity does not require reading
                // the file, and asking for read access would fail on anything
                // another process has open exclusively — which is exactly when
                // a file is most likely to be mid-save.
                0,
                // Share everything, for the same reason: this must never be
                // the handle that stops somebody else writing or deleting.
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                // Directories need the backup flag to open at all, and moving
                // a directory is exactly the case worth recognising.
                //
                // `OPEN_REPARSE_POINT` opens the link itself rather than what
                // it points at, so a junction cannot lend its target's
                // identity to something inside a protected folder.
                FILE_ATTRIBUTE_NORMAL | FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                None,
            )
            .ok()?;

            let mut info = BY_HANDLE_FILE_INFORMATION::default();
            let read = GetFileInformationByHandle(handle, &mut info).is_ok();
            let _ = CloseHandle(handle);
            if !read {
                return None;
            }

            let index = (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow);
            // A zero index means the filesystem does not supply one — FAT, and
            // some network redirectors. No identity is better than a shared
            // one that would make every such file look like the same file.
            if index == 0 {
                return None;
            }
            Some(format!("{:08x}:{index:016x}", info.dwVolumeSerialNumber))
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use std::path::Path;

    pub fn identify(_path: &Path) -> Option<super::FileIdentity> {
        None
    }
}

/// This file's identity on this machine, if the filesystem provides one.
///
/// `None` is an ordinary answer, not a failure: the file may have gone between
/// the event and the question, it may be on a filesystem that supplies no
/// index, or it may be locked in a way that refuses even an identity probe.
/// Every caller treats `None` as "cannot correlate this one" and falls back to
/// the path, which is always correct and merely less efficient.
pub fn identify(path: &std::path::Path) -> Option<FileIdentity> {
    imp::identify(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_has_an_identity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, b"hello").unwrap();
        assert!(identify(&path).is_some(), "NTFS should supply an index");
    }

    #[test]
    fn two_different_files_have_different_identities() {
        // The property the whole correlation rests on. If this ever failed,
        // moving one file could be reported as moving another.
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");
        std::fs::write(&a, b"same").unwrap();
        std::fs::write(&b, b"same").unwrap();
        assert_ne!(identify(&a), identify(&b));
    }

    #[test]
    fn identical_files_are_still_told_apart() {
        // Same name, same size, same content, same second — the case that
        // makes name/size/mtime matching unsafe. A folder of exported images
        // or build output is full of these.
        let dir = tempfile::tempdir().unwrap();
        let one = dir.path().join("one");
        let two = dir.path().join("two");
        std::fs::create_dir(&one).unwrap();
        std::fs::create_dir(&two).unwrap();
        std::fs::write(one.join("render.png"), b"identical bytes").unwrap();
        std::fs::write(two.join("render.png"), b"identical bytes").unwrap();
        assert_ne!(
            identify(&one.join("render.png")),
            identify(&two.join("render.png")),
        );
    }

    #[test]
    fn identity_survives_a_rename() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        std::fs::write(&a, b"hello").unwrap();
        let before = identify(&a);
        let b = dir.path().join("b.txt");
        std::fs::rename(&a, &b).unwrap();
        assert_eq!(before, identify(&b));
    }

    #[test]
    fn identity_survives_a_move_into_another_directory() {
        // The case this module exists for: no rename pair, and the file has to
        // be recognised where it landed.
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        std::fs::write(&a, b"hello").unwrap();
        let before = identify(&a);

        let sub = dir.path().join("Archive");
        std::fs::create_dir(&sub).unwrap();
        let moved = sub.join("a.txt");
        std::fs::rename(&a, &moved).unwrap();

        assert_eq!(before, identify(&moved));
        assert!(before.is_some());
    }

    #[test]
    fn identity_survives_a_directory_move() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("old");
        std::fs::create_dir(&from).unwrap();
        let before = identify(&from);
        let to = dir.path().join("new");
        std::fs::rename(&from, &to).unwrap();
        assert_eq!(before, identify(&to));
    }

    #[test]
    fn a_replacement_file_has_its_own_identity() {
        // The atomic-save shape: the path is the same, the file is not. If
        // identity followed the path rather than the file, a replacement would
        // be mistaken for the original still being there.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report.docx");
        std::fs::write(&path, b"version one").unwrap();
        let original = identify(&path);

        let temp = dir.path().join("~$report.tmp");
        std::fs::write(&temp, b"version two").unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::rename(&temp, &path).unwrap();

        assert_ne!(original, identify(&path));
    }

    #[test]
    fn a_path_that_is_not_there_has_no_identity() {
        let dir = tempfile::tempdir().unwrap();
        assert!(identify(&dir.path().join("never")).is_none());
    }

    #[test]
    fn identifying_a_file_does_not_lock_it() {
        // This probe runs on files somebody may be in the middle of saving. If
        // it took an exclusive handle it would make saving fail — turning a
        // backup client into the reason a document cannot be written.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("busy.txt");
        std::fs::write(&path, b"one").unwrap();

        let _ = identify(&path);
        std::fs::write(&path, b"two").expect("the file must still be writable");
        std::fs::remove_file(&path).expect("and still deletable");
    }
}
