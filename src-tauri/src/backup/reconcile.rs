//! Comparing what is on disk against what we believe, and queueing the
//! difference.
//!
//! # Why this exists even though there is a watcher
//!
//! Filesystem notifications are a hint that something changed, not a record of
//! what changed. They are dropped when the buffer overflows, they do not
//! arrive at all while the app is closed, they can be delivered out of order,
//! and a crash can lose whatever had not been written down yet. A design that
//! trusts them is a design that quietly drifts.
//!
//! So the watcher makes the common case fast, and this makes it correct. The
//! filesystem is the authority; this reads it and queues whatever the database
//! disagrees with. Run on launch, periodically, after waking, after
//! reconnecting, and whenever the watcher admits it lost events, it puts an
//! upper bound on how wrong things can get.
//!
//! # The one thing it must never do
//!
//! Infer deletions it is not sure about. Every other mistake here costs
//! bandwidth; this one costs somebody their backup. An unplugged drive, a
//! folder briefly locked, a permission error mid-walk — each presents as "all
//! the files are gone", and none of them means it. The guards below are the
//! most important code in this file.

use std::collections::HashMap;
use std::path::Path;

use crate::backup::scan::{self, CancelFlag, ScanProgress};
use crate::backup::store::{Entry, EntryType, Intent, Root, RootStatus, SyncState, SyncStore};
use crate::error::AppError;

/// Above this many disappearances, stop and ask rather than tombstone.
///
/// The number is a judgement, not a measurement: few people intentionally
/// delete five hundred files from a backed-up folder without knowing it, and
/// the cost of pausing when they did is a click, while the cost of proceeding
/// when they did not is their files leaving the server.
pub const MASS_DELETE_ABSOLUTE: i64 = 500;

/// …or this proportion of the folder, whichever is the smaller number.
///
/// The absolute limit alone would let a small folder be emptied entirely
/// without comment; the proportion alone would let a hundred-thousand-file
/// folder lose twenty thousand files quietly.
pub const MASS_DELETE_FRACTION: f64 = 0.25;

/// Below this many known entries, only the absolute limit applies.
///
/// A folder with four files in it hits "25%" by losing one, and one file is
/// exactly the deletion people make on purpose and expect to be honoured.
pub const MASS_DELETE_MIN_SAMPLE: i64 = 20;

/// What a pass over one folder concluded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The folder was read and any differences queued.
    Reconciled {
        created: usize,
        updated: usize,
        moved: usize,
        removed: usize,
    },
    /// The folder could not be read. **Nothing was queued**, and in particular
    /// nothing was treated as deleted.
    Unavailable,
    /// More disappeared than a person plausibly meant to delete. Nothing was
    /// queued; the folder is held until somebody confirms.
    MassChangeHeld { missing: i64, known: i64 },
}

/// Is a set of disappearances too large to act on without asking?
///
/// Split out and pure because it is the decision that matters most, and the
/// arithmetic deserves to be read on its own.
pub fn is_mass_deletion(missing: i64, known: i64) -> bool {
    if missing <= 0 {
        return false;
    }
    if missing >= MASS_DELETE_ABSOLUTE {
        return true;
    }
    if known >= MASS_DELETE_MIN_SAMPLE {
        return (missing as f64) >= (known as f64) * MASS_DELETE_FRACTION;
    }
    false
}

/// Can this folder be read at all right now?
///
/// `is_dir` rather than `exists`, and a trial read rather than trusting
/// either: a path can exist, report as a directory, and still refuse to be
/// enumerated — a disconnected network share, a drive that has spun down, a
/// folder whose permissions changed. Every one of those looks like an empty
/// folder to a walker that does not check.
pub fn root_is_readable(path: &Path) -> bool {
    if !path.is_dir() {
        return false;
    }
    match std::fs::read_dir(path) {
        // An empty folder is readable; the iterator simply yields nothing.
        // What matters is that opening it succeeded.
        Ok(_) => true,
        Err(err) => {
            tracing::info!(
                kind = ?err.kind(),
                "a protected folder could not be read; treating it as unavailable"
            );
            false
        }
    }
}

/// Bring one folder's queued work in line with what is actually on disk.
///
/// Reads nothing from the network and sends nothing. It only decides what the
/// engine should be asked to do, and writes that down in one transaction.
pub fn reconcile_root(
    store: &SyncStore,
    server_id: &str,
    root: &Root,
    cancel: &CancelFlag,
) -> Result<Outcome, AppError> {
    reconcile_root_inner(store, server_id, root, cancel, false)
}

/// The same pass, with the safety guard spent.
///
/// For one case only: the user has looked at the folder and said the files
/// really are gone. Clearing the hold on its own is not enough — the very next
/// pass would find the same disappearances and hold again, so confirming has
/// to be the thing that carries the removals out.
///
/// Still reads the disk first. If the files have come back in the meantime,
/// this removes nothing at all, which is the right answer to a confirmation
/// that turned out to be about a drive somebody has since plugged back in.
pub fn reconcile_root_confirming_removals(
    store: &SyncStore,
    server_id: &str,
    root: &Root,
    cancel: &CancelFlag,
) -> Result<Outcome, AppError> {
    reconcile_root_inner(store, server_id, root, cancel, true)
}

fn reconcile_root_inner(
    store: &SyncStore,
    server_id: &str,
    root: &Root,
    cancel: &CancelFlag,
    confirmed: bool,
) -> Result<Outcome, AppError> {
    if !root_is_readable(&root.local_path) {
        store.set_root_status(server_id, &root.id, RootStatus::Unavailable)?;
        return Ok(Outcome::Unavailable);
    }

    let scanned = scan::scan_root(&root.local_path, cancel, &ScanProgress::default());

    // A walk that was interrupted saw only part of the folder, and the part it
    // did not see is indistinguishable from files that are gone. Queue the
    // additions it found, never the removals it appears to imply.
    let partial = cancel.is_cancelled();

    // What is on disk now, by identity key so Windows' case-insensitivity is
    // handled the same way everywhere else handles it.
    let mut on_disk: HashMap<String, (EntryType, i64, i64)> = HashMap::new();
    for folder in &scanned.folders {
        on_disk.insert(
            crate::backup::protocol::path_identity_key(&folder.relative_path),
            (EntryType::Folder, 0, 0),
        );
    }
    for file in &scanned.files {
        on_disk.insert(
            crate::backup::protocol::path_identity_key(&file.relative_path),
            (EntryType::File, file.size_bytes as i64, file.modified_ms),
        );
    }

    let known = store.entries_in_root(server_id, &root.id)?;
    let mut known_by_key: HashMap<String, &Entry> = HashMap::new();
    for entry in &known {
        known_by_key.insert(
            crate::backup::protocol::path_identity_key(&entry.relative_path),
            entry,
        );
    }

    // Anything the database has that the disk does not. Collected before
    // anything is written, because the size of this set decides whether any of
    // it may be acted on.
    let missing: Vec<&Entry> = known
        .iter()
        .filter(|entry| entry.state != SyncState::Tombstoned)
        .filter(|entry| {
            !on_disk.contains_key(&crate::backup::protocol::path_identity_key(
                &entry.relative_path,
            ))
        })
        .collect();

    // Disappearances that are really moves.
    //
    // Matched on the file's own identity, which Windows guarantees is unique
    // on the volume and unchanged by a move. The obvious alternative — same
    // name, same size, same modification time — is unsafe rather than merely
    // imprecise: a folder of exported images or build output is full of files
    // agreeing on all three, and matching the wrong pair tells the server to
    // move one file on top of another. The file that loses is gone.
    //
    // Only files that arrived unrecognised are candidates, and each identity
    // is claimed once.
    let mut moves: Vec<(&Entry, String)> = Vec::new();
    let mut claimed: Vec<String> = Vec::new();

    let mut arrivals: HashMap<String, &str> = HashMap::new();
    for file in &scanned.files {
        let key = crate::backup::protocol::path_identity_key(&file.relative_path);
        if known_by_key.contains_key(&key) {
            continue;
        }
        if let Some(id) = crate::backup::identity::identify(
            &root.local_path.join(file.relative_path.replace('/', "\\")),
        ) {
            arrivals.insert(id, &file.relative_path);
        }
    }

    for gone in &missing {
        if gone.entry_type != EntryType::File || gone.synced_path.is_none() {
            continue;
        }
        let Some(file_id) = gone.file_id.as_deref() else {
            // Recorded before identities were kept, or on a filesystem that
            // supplies none. Not guessed at — it is treated as a deletion and
            // an unrelated arrival, which is correct, only less efficient.
            continue;
        };
        if let Some(landed) = arrivals.get(file_id) {
            let key = crate::backup::protocol::path_identity_key(landed);
            if claimed.contains(&key) {
                continue;
            }
            claimed.push(key);
            moves.push((gone, (*landed).to_string()));
        }
    }

    // Only the ones that are genuinely gone count toward the guard. A file
    // that moved has not been deleted, and counting it as one is how a tidy-up
    // of a big folder trips a safety hold it should not.
    let moved_away: Vec<&str> = moves
        .iter()
        .map(|(entry, _)| entry.client_entry_id.as_str())
        .collect();
    let deletions: Vec<&&Entry> = missing
        .iter()
        .filter(|entry| !moved_away.contains(&entry.client_entry_id.as_str()))
        .collect();

    let known_count = store.entry_count_in_root(server_id, &root.id)?;
    let hold = !partial && !confirmed && is_mass_deletion(deletions.len() as i64, known_count);
    if hold {
        tracing::warn!(
            server_id,
            root_id = %root.id,
            missing = deletions.len(),
            known = known_count,
            "an unusually large number of files disappeared; holding this folder"
        );
        store.set_root_status(server_id, &root.id, RootStatus::SafetyHold)?;
        return Ok(Outcome::MassChangeHeld {
            missing: deletions.len() as i64,
            known: known_count,
        });
    }

    // Additions and changes.
    let mut batch: Vec<Entry> = Vec::new();
    let mut created = 0usize;
    let mut updated = 0usize;

    let mut consider = |relative_path: &str, entry_type: EntryType, size: i64, modified: i64| {
        let key = crate::backup::protocol::path_identity_key(relative_path);
        if claimed.contains(&key) {
            // Already accounted for as the destination of a move.
            return;
        }
        match known_by_key.get(&key) {
            // Nothing to say about this one.
            //
            // Two ways that can be true, and both matter. Either the server
            // already has exactly this content, or it is already queued to
            // receive exactly this content — and re-queueing work that is
            // already queued is not free: during the initial backup of a large
            // folder every entry is pending, so a pass that rewrote them all
            // would rewrite ten thousand rows every ten minutes to change
            // nothing.
            //
            // `FAILED` is deliberately absent: a failure is worth retrying,
            // and this is the pass that retries it.
            Some(entry)
                if entry.size_bytes == size
                    && entry.modified_ms == modified
                    && (entry.state == SyncState::Synced
                        || (entry.state == SyncState::Pending
                            && entry.intent == Intent::Upsert)) => {}
            Some(entry) => {
                updated += 1;
                batch.push(Entry {
                    client_entry_id: entry.client_entry_id.clone(),
                    root_id: root.id.clone(),
                    relative_path: relative_path.to_string(),
                    entry_type,
                    size_bytes: size,
                    modified_ms: modified,
                    state: SyncState::Pending,
                    pending_operation_id: entry.pending_operation_id.clone(),
                    intent: Intent::Upsert,
                    synced_path: entry.synced_path.clone(),
                    file_id: crate::backup::identity::identify(
                        &root.local_path.join(relative_path.replace('/', "\\")),
                    ),
                });
            }
            None => {
                created += 1;
                batch.push(Entry {
                    client_entry_id: uuid::Uuid::new_v4().to_string(),
                    root_id: root.id.clone(),
                    relative_path: relative_path.to_string(),
                    entry_type,
                    size_bytes: size,
                    modified_ms: modified,
                    state: SyncState::Pending,
                    pending_operation_id: None,
                    intent: Intent::Upsert,
                    synced_path: None,
                    file_id: crate::backup::identity::identify(
                        &root.local_path.join(relative_path.replace('/', "\\")),
                    ),
                });
            }
        }
    };

    for folder in &scanned.folders {
        consider(&folder.relative_path, EntryType::Folder, 0, 0);
    }
    for file in &scanned.files {
        consider(
            &file.relative_path,
            EntryType::File,
            file.size_bytes as i64,
            file.modified_ms,
        );
    }
    // `consider` borrowed `batch` mutably; letting it go here is what makes
    // the batch usable again below.
    let _ = consider;

    store.apply_batch(server_id, &batch)?;

    for (entry, new_path) in &moves {
        store.queue_move(server_id, &entry.client_entry_id, new_path)?;
    }

    let mut removed = 0usize;
    if !partial {
        for entry in &deletions {
            if store.queue_tombstone(server_id, &entry.client_entry_id)? {
                removed += 1;
            }
        }
    }

    // Reading it successfully is itself the news that it is available again.
    if root.status != RootStatus::Active {
        store.set_root_status(server_id, &root.id, RootStatus::Active)?;
    }

    Ok(Outcome::Reconciled {
        created,
        updated,
        moved: moves.len(),
        removed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- The guard --------------------------------------------------------
    //
    // These are the numbers that decide whether a mistake costs bandwidth or
    // costs somebody their files.

    #[test]
    fn nothing_missing_is_not_a_mass_deletion() {
        assert!(!is_mass_deletion(0, 1000));
    }

    #[test]
    fn deleting_a_few_files_is_allowed_through() {
        // The ordinary case, and it must stay ordinary: people delete files.
        assert!(!is_mass_deletion(1, 1000));
        assert!(!is_mass_deletion(50, 1000));
    }

    #[test]
    fn five_hundred_disappearances_are_held_however_big_the_folder() {
        // Half a million files and five hundred gone is only a tenth of a
        // percent, but five hundred is already more than anybody deletes
        // without noticing.
        assert!(is_mass_deletion(MASS_DELETE_ABSOLUTE, 500_000));
    }

    #[test]
    fn a_quarter_of_a_folder_is_held_even_when_the_count_is_small() {
        // 30 of 100 is not a tidy-up, it is something going wrong.
        assert!(is_mass_deletion(30, 100));
    }

    #[test]
    fn a_tiny_folder_is_judged_only_on_the_absolute_limit() {
        // Four files and one deleted is 25%, and it is also just deleting a
        // file. Holding here would make the guard fire constantly on exactly
        // the folders where it matters least.
        assert!(!is_mass_deletion(1, 4));
        assert!(!is_mass_deletion(3, 10));
    }

    #[test]
    fn an_empty_folder_cannot_trip_the_guard() {
        assert!(!is_mass_deletion(0, 0));
    }

    #[test]
    fn the_whole_folder_disappearing_is_always_held() {
        // The unplugged-drive shape. Even if the root itself still reports as
        // readable, losing everything is never a thing to act on unasked.
        assert!(is_mass_deletion(1000, 1000));
        assert!(is_mass_deletion(25, 25));
    }

    // --- Readability ------------------------------------------------------

    #[test]
    fn a_folder_that_is_there_is_readable() {
        let dir = tempfile::tempdir().unwrap();
        assert!(root_is_readable(dir.path()));
    }

    #[test]
    fn an_empty_folder_is_readable_not_missing() {
        // The distinction that matters: an empty folder yields no entries, and
        // must not be confused with one that cannot be opened. Read as
        // unavailable, a genuinely emptied folder would never sync its
        // deletions; read as readable, it reconciles correctly.
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        assert!(root_is_readable(dir.path()));
    }

    #[test]
    fn a_folder_that_is_gone_is_not_readable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vanished");
        std::fs::create_dir(&path).unwrap();
        assert!(root_is_readable(&path));
        std::fs::remove_dir(&path).unwrap();
        assert!(!root_is_readable(&path));
    }

    #[test]
    fn a_file_where_a_folder_was_is_not_readable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("not-a-folder");
        std::fs::write(&path, b"x").unwrap();
        assert!(!root_is_readable(&path));
    }

    #[test]
    fn a_drive_that_is_not_mounted_is_not_readable() {
        // The shape of the incident this all exists to prevent: a path that
        // simply is not there any more must never read as "everything in it
        // was deleted".
        assert!(!root_is_readable(Path::new(r"Q:\NoSuchVolume\Protected")));
    }
}

// --- Acting on one watcher change -----------------------------------------

/// Apply one normalised change to the queue.
///
/// The event says where to look; this looks, and writes down what should
/// happen. Nothing here trusts the event's own account of what occurred — a
/// `Touched` for a file that has since been deleted is a deletion, and a
/// `Gone` for something still on disk is nothing at all.
pub fn apply_change(
    store: &SyncStore,
    server_id: &str,
    root: &Root,
    change: &crate::backup::watcher::LocalChange,
) -> Result<(), AppError> {
    use crate::backup::watcher::LocalChange;

    // A folder nobody can read, or one being held, takes no watcher work. The
    // events are not wrong; it is simply not the moment to act on them.
    if !root.enabled || root.status != RootStatus::Active {
        return Ok(());
    }

    match change {
        LocalChange::Touched { relative, .. } => touched(store, server_id, root, relative),
        LocalChange::Gone { relative, .. } => gone(store, server_id, root, relative),
        LocalChange::Renamed { from, to, .. } => renamed(store, server_id, root, from, to),
        // Handled by the caller, which owns the decision to walk a folder.
        LocalChange::RescanNeeded { .. } => Ok(()),
    }
}

fn absolute(root: &Root, relative: &str) -> std::path::PathBuf {
    root.local_path.join(relative.replace('/', "\\"))
}

fn touched(
    store: &SyncStore,
    server_id: &str,
    root: &Root,
    relative: &str,
) -> Result<(), AppError> {
    let path = absolute(root, relative);

    let Ok(metadata) = std::fs::symlink_metadata(&path) else {
        // It was there when the event fired and is not there now. The event is
        // stale; what is true is that it is gone.
        return gone(store, server_id, root, relative);
    };

    // The scanner refuses to follow reparse points, and so must this. A
    // junction pointing back up the tree is how a backup walks forever, and a
    // symlink out of the folder is how it escapes the folder entirely.
    if crate::backup::scan::is_reparse_point(&metadata) {
        tracing::info!("a reparse point changed inside a protected folder; not following it");
        return Ok(());
    }

    // Windows' own answer to "is this a file we already have?".
    let file_id = crate::backup::identity::identify(&path);

    let existing = store.entry_by_path(server_id, &root.id, relative)?;

    // Nothing recorded at this path, but this exact file is recorded at
    // another one — so it moved, and the server should be told to move it
    // rather than sent the bytes again.
    //
    // This is what covers a move across directories, which Windows does not
    // always report as a rename pair, and a move discovered after the fact
    // because the app was closed when it happened.
    if existing.is_none() {
        if let Some(file_id) = file_id.as_deref() {
            if let Some(elsewhere) = store.entry_by_file_id(server_id, &root.id, file_id)? {
                let gone_from_there = !absolute(root, &elsewhere.relative_path).exists();
                if gone_from_there && elsewhere.relative_path != relative {
                    if elsewhere.entry_type == EntryType::Folder {
                        store.queue_subtree_move(
                            server_id,
                            &root.id,
                            &elsewhere.relative_path,
                            relative,
                        )?;
                    } else {
                        store.queue_move(server_id, &elsewhere.client_entry_id, relative)?;
                    }
                    return Ok(());
                }
            }
        }
    }

    let (entry_type, size, modified) = if metadata.is_dir() {
        (EntryType::Folder, 0, 0)
    } else {
        (
            EntryType::File,
            metadata.len() as i64,
            crate::backup::scan::modified_ms(&metadata),
        )
    };

    // Already sent, and nothing about it has changed. Watchers fire for
    // reasons that are not changes — an attribute touched, a folder opened —
    // and re-uploading on each would cost the user bandwidth to tell Arciin
    // something it already knows.
    if let Some(entry) = &existing {
        if entry.state == SyncState::Synced
            && entry.size_bytes == size
            && entry.modified_ms == modified
        {
            return Ok(());
        }
    }

    let entry = Entry {
        client_entry_id: existing
            .as_ref()
            .map(|e| e.client_entry_id.clone())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        root_id: root.id.clone(),
        relative_path: relative.to_string(),
        entry_type,
        size_bytes: size,
        modified_ms: modified,
        state: SyncState::Pending,
        pending_operation_id: existing
            .as_ref()
            .and_then(|e| e.pending_operation_id.clone()),
        intent: Intent::Upsert,
        synced_path: existing.and_then(|e| e.synced_path),
        file_id,
    };

    // Ancestors first, and in the same write. A file whose parent folder has
    // never been sent cannot be created on the server, and queueing the file
    // without the folder is how an upload fails for a reason the user cannot
    // act on.
    let mut batch = ancestor_entries(store, server_id, root, relative)?;
    batch.push(entry);
    store.apply_batch(server_id, &batch)
}

/// Queue any of this path's parent folders the database does not know about.
///
/// Normally none: the watcher reports the folder's creation before the file's.
/// It is the abnormal cases this covers — a folder tree created faster than
/// the events describing it, a dropped event, a file that arrived by a move
/// whose parent was never separately announced.
fn ancestor_entries(
    store: &SyncStore,
    server_id: &str,
    root: &Root,
    relative: &str,
) -> Result<Vec<Entry>, AppError> {
    let mut out = Vec::new();
    for ancestor in crate::backup::ordering::ancestors(relative) {
        if store
            .entry_by_path(server_id, &root.id, &ancestor)?
            .is_some()
        {
            continue;
        }
        if !absolute(root, &ancestor).is_dir() {
            continue;
        }
        let ancestor_path = ancestor.clone();
        out.push(Entry {
            client_entry_id: uuid::Uuid::new_v4().to_string(),
            root_id: root.id.clone(),
            relative_path: ancestor,
            entry_type: EntryType::Folder,
            size_bytes: 0,
            modified_ms: 0,
            state: SyncState::Pending,
            pending_operation_id: None,
            intent: Intent::Upsert,
            synced_path: None,
            file_id: crate::backup::identity::identify(&absolute(root, &ancestor_path)),
        });
    }
    Ok(out)
}

fn gone(store: &SyncStore, server_id: &str, root: &Root, relative: &str) -> Result<(), AppError> {
    // Still there. Either the event was about something transient that has
    // since come back — an atomic save replaces a file by deleting it — or it
    // was simply wrong. Either way there is nothing to remove.
    if absolute(root, relative).exists() {
        return touched(store, server_id, root, relative);
    }

    let Some(entry) = store.entry_by_path(server_id, &root.id, relative)? else {
        return Ok(());
    };

    if entry.entry_type == EntryType::Folder {
        // The same guard the reconciler applies, because the same catastrophe
        // arrives down this path too — and arrives faster.
        //
        // A folder disappearing produces one event, and acting on it removes
        // everything beneath it. That is correct when somebody deleted a
        // folder and wrong in every other case that looks identical from here:
        // a drive unmounting, a network share dropping, a sync client from
        // somewhere else clearing a directory it thought it owned. The
        // reconciler alone was not enough — it only runs afterwards, by which
        // point the removals have already been sent.
        let doomed = store.entries_under(server_id, &root.id, relative)?.len() as i64;
        let known = store.entry_count_in_root(server_id, &root.id)?;
        if is_mass_deletion(doomed, known) {
            tracing::warn!(
                root_id = %root.id,
                doomed,
                known,
                "a folder holding an unusually large share of this backup disappeared; holding"
            );
            store.set_root_status(server_id, &root.id, RootStatus::SafetyHold)?;
            return Ok(());
        }

        let queued = store.queue_subtree_tombstone(server_id, &root.id, relative)?;
        tracing::info!(entries = queued, "a protected folder was removed locally");
        return Ok(());
    }

    store.queue_tombstone(server_id, &entry.client_entry_id)?;
    Ok(())
}

fn renamed(
    store: &SyncStore,
    server_id: &str,
    root: &Root,
    from: &str,
    to: &str,
) -> Result<(), AppError> {
    // Windows is case-insensitive, and so is this client's idea of identity.
    // `report.txt` becoming `Report.txt` is the same entry with a different
    // spelling — not a move, and certainly not a second file.
    let same_place = crate::backup::protocol::path_identity_key(from)
        == crate::backup::protocol::path_identity_key(to);

    let Some(entry) = store.entry_by_path(server_id, &root.id, from)? else {
        // Nothing known at the old path. Whatever arrived is simply new.
        return touched(store, server_id, root, to);
    };

    if same_place {
        // Record the new spelling without asking the server to move anything
        // to where it already is.
        let mut renamed = entry.clone();
        renamed.relative_path = to.to_string();
        return store.apply_batch(server_id, &[renamed]);
    }

    // Something is already recorded at the destination, and it is not this.
    //
    // This is the ordinary Windows save, not an edge case: write a temporary
    // file, delete the original, rename the temporary into its place. The
    // temporary's entry is now being moved onto a path another entry already
    // holds — a collision on the one-entry-per-path rule, and the reason this
    // whole branch exists.
    //
    // Resolved by keeping the destination's identity rather than the
    // temporary's. The user has one document with a history, and the server
    // should go on seeing one document with a history, rather than watching
    // the original vanish and a file with a scratch name take its place.
    if let Some(occupant) = store.entry_by_path(server_id, &root.id, to)? {
        if occupant.client_entry_id != entry.client_entry_id {
            store.queue_tombstone(server_id, &entry.client_entry_id)?;
            return touched(store, server_id, root, to);
        }
    }

    if entry.entry_type == EntryType::Folder {
        let moved = store.queue_subtree_move(server_id, &root.id, from, to)?;
        tracing::info!(entries = moved, "a protected folder was renamed locally");
        return Ok(());
    }

    store.queue_move(server_id, &entry.client_entry_id, to)
}
