//! Walking a protected folder.
//!
//! Used twice: once to estimate a root's size before the user commits to
//! backing it up, and again to enumerate entries for upload.
//!
//! A scan crosses a real user's disk, so it must survive everything a real
//! disk does — files that vanish mid-walk, directories the account cannot
//! open, junctions that loop back on themselves, paths past `MAX_PATH`. None
//! of those are exceptional here; they are Tuesday. A scan therefore never
//! fails as a whole. It skips what it cannot read, counts what it skipped, and
//! keeps going.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use walkdir::WalkDir;

use crate::backup::protocol::normalize_relative_path;

/// Names never worth sending. Deliberately short.
///
/// The rule is *technical* exclusion only: things that are not user content,
/// cannot be read meaningfully, or are an artefact of the filesystem itself.
/// User files are never excluded by extension — a `.tmp` someone deliberately
/// saved is still their file.
const EXCLUDED_NAMES: &[&str] = &[
    // Windows filesystem bookkeeping.
    "$RECYCLE.BIN",
    "System Volume Information",
    "Thumbs.db",
    "desktop.ini",
    "ehthumbs.db",
    // macOS metadata that rides along on shared drives.
    ".DS_Store",
];

/// A file found by a scan.
#[derive(Debug, Clone)]
pub struct ScannedFile {
    pub relative_path: String,
    pub absolute_path: PathBuf,
    pub size_bytes: u64,
    pub modified_ms: i64,
}

/// A folder found by a scan.
#[derive(Debug, Clone)]
pub struct ScannedFolder {
    pub relative_path: String,
    pub absolute_path: PathBuf,
}

/// What a completed (or cancelled) scan found.
#[derive(Debug, Default, Clone)]
pub struct ScanResult {
    pub files: Vec<ScannedFile>,
    pub folders: Vec<ScannedFolder>,
    pub total_bytes: u64,
    /// Entries skipped, with why, so the UI can be honest about coverage
    /// rather than silently under-reporting.
    pub skipped_unreadable: u64,
    pub skipped_reparse: u64,
    pub skipped_path_invalid: u64,
    /// True when a cancellation stopped the walk early.
    pub cancelled: bool,
}

/// Just the numbers, for the folder-selection screen.
#[derive(Debug, Default, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanSummary {
    pub file_count: u64,
    pub folder_count: u64,
    pub total_bytes: u64,
    pub skipped: u64,
    pub cancelled: bool,
}

/// Lets a long walk be abandoned when the user moves on.
#[derive(Clone, Default)]
pub struct CancelFlag(Arc<AtomicBool>);

impl CancelFlag {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// Live counters a UI can poll while a scan runs.
#[derive(Clone, Default)]
pub struct ScanProgress {
    pub files: Arc<AtomicU64>,
    pub bytes: Arc<AtomicU64>,
}

/// Is this a reparse point (symlink, junction, mount point, cloud placeholder)?
///
/// These are skipped by default. A junction can point back up its own tree, so
/// following one risks walking forever or backing up a folder twice under two
/// identities. The protocol is explicit that the client skips them and the
/// server must not assume otherwise.
#[cfg(windows)]
pub fn is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
pub fn is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    metadata.is_symlink()
}

fn excluded_name(name: &str) -> bool {
    EXCLUDED_NAMES
        .iter()
        .any(|banned| banned.eq_ignore_ascii_case(name))
}

pub fn modified_ms(metadata: &std::fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Walk `root`, collecting files and folders.
///
/// `progress` is updated as it goes; `cancel` is checked per entry so a scan
/// of a huge tree stops promptly.
pub fn scan_root(root: &Path, cancel: &CancelFlag, progress: &ScanProgress) -> ScanResult {
    let mut result = ScanResult::default();

    let walker = WalkDir::new(root)
        // Never traverse a link. `walkdir` would otherwise follow junctions.
        .follow_links(false)
        // Deterministic order, and it keeps a parent ahead of its children so
        // folders are created before the files inside them.
        .sort_by_file_name()
        .into_iter();

    // `filter_entry` prunes a directory *before* descending into it, which is
    // what keeps an excluded or linked folder from being walked at all.
    let walker = walker.filter_entry(|entry| {
        if entry.depth() == 0 {
            return true;
        }
        let name = entry.file_name().to_string_lossy();
        if excluded_name(&name) {
            return false;
        }
        match entry.metadata() {
            Ok(metadata) => !is_reparse_point(&metadata),
            // Unreadable: let the main loop record it as skipped.
            Err(_) => true,
        }
    });

    for entry in walker {
        if cancel.is_cancelled() {
            result.cancelled = true;
            break;
        }

        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                // Permission denied, a file deleted mid-walk, a path too long.
                // The path is not logged: it is the user's private layout.
                tracing::debug!(kind = ?err.io_error().map(|e| e.kind()), "scan skipped an entry");
                result.skipped_unreadable += 1;
                continue;
            }
        };

        if entry.depth() == 0 {
            continue;
        }

        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(_) => {
                result.skipped_unreadable += 1;
                continue;
            }
        };

        if is_reparse_point(&metadata) {
            result.skipped_reparse += 1;
            continue;
        }

        let Ok(relative) = entry.path().strip_prefix(root) else {
            result.skipped_path_invalid += 1;
            continue;
        };
        let Some(relative_path) = normalize_relative_path(&relative.to_string_lossy()) else {
            // Too long, too deep, or a name the server will not accept. Counted
            // so the UI can say so rather than quietly dropping it.
            result.skipped_path_invalid += 1;
            continue;
        };

        if metadata.is_dir() {
            result.folders.push(ScannedFolder {
                relative_path,
                absolute_path: entry.path().to_path_buf(),
            });
        } else if metadata.is_file() {
            let size = metadata.len();
            result.total_bytes += size;
            progress.files.fetch_add(1, Ordering::Relaxed);
            progress.bytes.fetch_add(size, Ordering::Relaxed);
            result.files.push(ScannedFile {
                relative_path,
                absolute_path: entry.path().to_path_buf(),
                size_bytes: size,
                modified_ms: modified_ms(&metadata),
            });
        }
    }

    result
}

/// Size and count only, without retaining the entry list.
///
/// The selection screen needs six of these at once; holding every path for six
/// large trees would cost far more memory than the numbers are worth.
pub fn summarize_root(root: &Path, cancel: &CancelFlag) -> ScanSummary {
    let mut summary = ScanSummary::default();

    let walker = WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            if entry.depth() == 0 {
                return true;
            }
            let name = entry.file_name().to_string_lossy();
            if excluded_name(&name) {
                return false;
            }
            match entry.metadata() {
                Ok(metadata) => !is_reparse_point(&metadata),
                Err(_) => true,
            }
        });

    for entry in walker {
        if cancel.is_cancelled() {
            summary.cancelled = true;
            break;
        }
        let Ok(entry) = entry else {
            summary.skipped += 1;
            continue;
        };
        if entry.depth() == 0 {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            summary.skipped += 1;
            continue;
        };
        if is_reparse_point(&metadata) {
            summary.skipped += 1;
        } else if metadata.is_dir() {
            summary.folder_count += 1;
        } else if metadata.is_file() {
            summary.file_count += 1;
            summary.total_bytes += metadata.len();
        }
    }

    summary
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn fixture() -> tempfile::TempDir {
        // The tree from the task's Phase 33 fixture.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("photo.jpg"), b"jpeg-bytes").unwrap();
        fs::write(root.join("invoice.pdf"), b"pdf-bytes!!").unwrap();
        fs::write(root.join("video.mp4"), b"mp4").unwrap();

        let project = root.join("WebProject");
        fs::create_dir(&project).unwrap();
        fs::write(project.join("package.json"), b"{}").unwrap();
        fs::write(project.join("README.md"), b"# hi").unwrap();
        fs::write(project.join("demo.mp4"), b"demo").unwrap();

        let public = project.join("public");
        fs::create_dir(&public).unwrap();
        fs::write(public.join("logo.png"), b"png").unwrap();
        dir
    }

    #[test]
    fn the_whole_tree_is_found() {
        let dir = fixture();
        let result = scan_root(dir.path(), &CancelFlag::new(), &ScanProgress::default());

        assert_eq!(result.files.len(), 7);
        assert_eq!(result.folders.len(), 2);
        assert!(!result.cancelled);
    }

    #[test]
    fn the_project_tree_stays_together() {
        // The product rule: nothing is flattened or split by media type. The
        // relative paths must mirror the source hierarchy exactly.
        let dir = fixture();
        let result = scan_root(dir.path(), &CancelFlag::new(), &ScanProgress::default());

        let paths: Vec<&str> = result
            .files
            .iter()
            .map(|f| f.relative_path.as_str())
            .collect();
        assert!(paths.contains(&"WebProject/public/logo.png"));
        assert!(paths.contains(&"WebProject/demo.mp4"));
        assert!(paths.contains(&"WebProject/package.json"));
        assert!(paths.contains(&"photo.jpg"));

        // A media file deep in a project keeps its place; it is not hoisted.
        assert!(!paths.contains(&"logo.png"));
        assert!(!paths.contains(&"demo.mp4"));
    }

    #[test]
    fn paths_use_forward_slashes() {
        let dir = fixture();
        let result = scan_root(dir.path(), &CancelFlag::new(), &ScanProgress::default());
        for file in &result.files {
            assert!(!file.relative_path.contains('\\'), "{}", file.relative_path);
            assert!(!file.relative_path.starts_with('/'));
        }
    }

    #[test]
    fn folders_are_listed_before_their_contents() {
        let dir = fixture();
        let result = scan_root(dir.path(), &CancelFlag::new(), &ScanProgress::default());
        let public = result
            .folders
            .iter()
            .position(|f| f.relative_path == "WebProject/public")
            .unwrap();
        let parent = result
            .folders
            .iter()
            .position(|f| f.relative_path == "WebProject")
            .unwrap();
        assert!(parent < public, "a parent must be created before its child");
    }

    #[test]
    fn sizes_add_up() {
        let dir = fixture();
        let result = scan_root(dir.path(), &CancelFlag::new(), &ScanProgress::default());
        let expected: u64 = result.files.iter().map(|f| f.size_bytes).sum();
        assert_eq!(result.total_bytes, expected);
        assert!(result.total_bytes > 0);
    }

    #[test]
    fn the_summary_matches_a_full_scan() {
        let dir = fixture();
        let full = scan_root(dir.path(), &CancelFlag::new(), &ScanProgress::default());
        let summary = summarize_root(dir.path(), &CancelFlag::new());

        assert_eq!(summary.file_count, full.files.len() as u64);
        assert_eq!(summary.folder_count, full.folders.len() as u64);
        assert_eq!(summary.total_bytes, full.total_bytes);
    }

    #[test]
    fn windows_housekeeping_files_are_skipped() {
        let dir = fixture();
        fs::write(dir.path().join("Thumbs.db"), b"x").unwrap();
        fs::write(dir.path().join("desktop.ini"), b"x").unwrap();

        let result = scan_root(dir.path(), &CancelFlag::new(), &ScanProgress::default());
        let paths: Vec<&str> = result
            .files
            .iter()
            .map(|f| f.relative_path.as_str())
            .collect();
        assert!(!paths.iter().any(|p| p.eq_ignore_ascii_case("Thumbs.db")));
        assert!(!paths.iter().any(|p| p.eq_ignore_ascii_case("desktop.ini")));
    }

    #[test]
    fn ordinary_user_files_are_never_excluded_by_extension() {
        // Only technical artefacts are skipped. A .tmp or .log the user saved
        // is still their file.
        let dir = fixture();
        fs::write(dir.path().join("notes.tmp"), b"mine").unwrap();
        fs::write(dir.path().join("debug.log"), b"mine").unwrap();
        fs::write(dir.path().join(".env"), b"mine").unwrap();

        let result = scan_root(dir.path(), &CancelFlag::new(), &ScanProgress::default());
        let paths: Vec<&str> = result
            .files
            .iter()
            .map(|f| f.relative_path.as_str())
            .collect();
        assert!(paths.contains(&"notes.tmp"));
        assert!(paths.contains(&"debug.log"));
        assert!(paths.contains(&".env"));
    }

    #[test]
    fn cancelling_stops_the_walk() {
        let dir = fixture();
        let cancel = CancelFlag::new();
        cancel.cancel();
        let result = scan_root(dir.path(), &cancel, &ScanProgress::default());
        assert!(result.cancelled);
        assert!(result.files.is_empty());
    }

    #[test]
    fn an_empty_root_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let result = scan_root(dir.path(), &CancelFlag::new(), &ScanProgress::default());
        assert!(result.files.is_empty());
        assert!(!result.cancelled);
    }

    #[test]
    fn progress_counts_while_it_walks() {
        let dir = fixture();
        let progress = ScanProgress::default();
        let result = scan_root(dir.path(), &CancelFlag::new(), &progress);
        assert_eq!(
            progress.files.load(Ordering::Relaxed),
            result.files.len() as u64
        );
        assert_eq!(progress.bytes.load(Ordering::Relaxed), result.total_bytes);
    }
}
