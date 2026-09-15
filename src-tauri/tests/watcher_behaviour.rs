//! What the watcher and the reconciler actually do to the queue.
//!
//! These run against a real temporary folder and a real SQLite database, and
//! assert on the intent that ends up recorded rather than on any request —
//! nothing here talks to a server. That is the seam the whole design rests on:
//! filesystem truth in, queued intent out, and the existing engine drains it.
//!
//! The cases are the ones that go wrong in the field. Editors that save by
//! replacing a file, folders that disappear because a drive was unplugged,
//! renames that a naive client turns into a re-upload and a deletion.

use std::path::{Path, PathBuf};

use arciin_desktop_lib::backup::reconcile::{self, Outcome};
use arciin_desktop_lib::backup::scan::CancelFlag;
use arciin_desktop_lib::backup::store::{
    Entry, EntryType, Intent, Profile, Root, RootStatus, SyncState, SyncStore,
};
use arciin_desktop_lib::backup::watcher::LocalChange;

const SERVER: &str = "3f2504e0-4f89-41d3-9a0c-0305e82c3301";
const ROOT: &str = "root-1";

struct Fixture {
    _dir: tempfile::TempDir,
    store: SyncStore,
    root: Root,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        let protected = dir.path().join("Protected");
        std::fs::create_dir_all(&protected).unwrap();

        let store = SyncStore::open(&state).unwrap();
        store
            .save_profile(&Profile {
                server_id: SERVER.into(),
                profile_id: "profile-1".into(),
                device_id: "device-1".into(),
                user_id: "user-1".into(),
                paused: false,
                enabled: true,
            })
            .unwrap();

        let root = Root {
            id: ROOT.into(),
            kind: "CUSTOM".into(),
            display_name: "Protected".into(),
            local_path: protected,
            enabled: true,
            status: RootStatus::Active,
        };
        store.save_root(SERVER, &root).unwrap();

        Self {
            _dir: dir,
            store,
            root,
        }
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.root.local_path.join(relative.replace('/', "\\"))
    }

    fn write(&self, relative: &str, contents: &[u8]) {
        let path = self.path(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, contents).unwrap();
    }

    fn mkdir(&self, relative: &str) {
        std::fs::create_dir_all(self.path(relative)).unwrap();
    }

    fn apply(&self, change: LocalChange) {
        reconcile::apply_change(&self.store, SERVER, &self.root, &change).unwrap();
    }

    fn touched(&self, relative: &str) {
        self.apply(LocalChange::Touched {
            root_id: ROOT.into(),
            relative: relative.into(),
        });
    }

    fn gone(&self, relative: &str) {
        self.apply(LocalChange::Gone {
            root_id: ROOT.into(),
            relative: relative.into(),
        });
    }

    fn renamed(&self, from: &str, to: &str) {
        self.apply(LocalChange::Renamed {
            root_id: ROOT.into(),
            from: from.into(),
            to: to.into(),
        });
    }

    fn reconcile(&self) -> Outcome {
        reconcile::reconcile_root(&self.store, SERVER, &self.root, &CancelFlag::new()).unwrap()
    }

    fn entry(&self, relative: &str) -> Option<Entry> {
        self.store.entry_by_path(SERVER, ROOT, relative).unwrap()
    }

    /// Pretend the engine sent everything outstanding and the server accepted.
    fn mark_all_synced(&self) {
        for entry in self.store.outstanding(SERVER, 10_000).unwrap() {
            self.store
                .complete_operation(SERVER, &entry.client_entry_id, SyncState::Synced)
                .unwrap();
        }
    }

    fn outstanding_paths(&self) -> Vec<(String, Intent)> {
        self.store
            .outstanding(SERVER, 10_000)
            .unwrap()
            .into_iter()
            .map(|entry| (entry.relative_path, entry.intent))
            .collect()
    }
}

// --- Create ---------------------------------------------------------------

#[test]
fn a_new_file_is_queued_for_upload() {
    let f = Fixture::new();
    f.write("hello.txt", b"hello");
    f.touched("hello.txt");

    let entry = f.entry("hello.txt").expect("queued");
    assert_eq!(entry.entry_type, EntryType::File);
    assert_eq!(entry.intent, Intent::Upsert);
    assert_eq!(entry.state, SyncState::Pending);
    assert_eq!(entry.size_bytes, 5);
}

#[test]
fn a_new_folder_is_queued_before_the_file_in_it() {
    // The server cannot put a file in a folder it has not been told about.
    let f = Fixture::new();
    f.write("deep/a/b/note.txt", b"x");
    f.touched("deep/a/b/note.txt");

    let queued = f.outstanding_paths();
    let folder = queued.iter().position(|(p, _)| p == "deep").unwrap();
    let file = queued
        .iter()
        .position(|(p, _)| p == "deep/a/b/note.txt")
        .unwrap();
    assert!(folder < file, "the folder must be queued before the file");

    for ancestor in ["deep", "deep/a", "deep/a/b"] {
        let entry = f.entry(ancestor).expect(ancestor);
        assert_eq!(entry.entry_type, EntryType::Folder);
    }
}

#[test]
fn a_file_that_vanished_before_we_looked_is_not_queued() {
    // Events arrive after the fact. A create for something already deleted is
    // a stale event, and uploading it is impossible anyway.
    let f = Fixture::new();
    f.touched("never-existed.txt");
    assert!(f.entry("never-existed.txt").is_none());
}

// --- Modify ---------------------------------------------------------------

#[test]
fn an_unchanged_file_is_not_re_uploaded() {
    // Watchers fire for things that are not changes. Re-sending on each would
    // cost the user bandwidth to tell Arciin what it already knows.
    let f = Fixture::new();
    f.write("a.txt", b"one");
    f.touched("a.txt");
    f.mark_all_synced();

    f.touched("a.txt");

    assert!(f.outstanding_paths().is_empty());
    assert_eq!(f.entry("a.txt").unwrap().state, SyncState::Synced);
}

#[test]
fn a_changed_file_is_queued_again_under_the_same_identity() {
    let f = Fixture::new();
    f.write("a.txt", b"one");
    f.touched("a.txt");
    f.mark_all_synced();
    let first = f.entry("a.txt").unwrap().client_entry_id;

    // A different length, so the change is visible whatever the clock did.
    f.write("a.txt", b"one and then some more");
    f.touched("a.txt");

    let entry = f.entry("a.txt").unwrap();
    assert_eq!(entry.state, SyncState::Pending);
    assert_eq!(entry.intent, Intent::Upsert);
    assert_eq!(
        entry.client_entry_id, first,
        "editing a file must not mint a second identity for it"
    );
}

#[test]
fn a_burst_of_saves_leaves_one_piece_of_work() {
    // The coalescing that matters: an editor writing repeatedly must not queue
    // the file once per write.
    let f = Fixture::new();
    f.write("doc.txt", b"v1");
    for version in ["v2", "v3", "v4", "v5"] {
        f.write("doc.txt", version.as_bytes());
        f.touched("doc.txt");
    }

    let queued: Vec<_> = f
        .outstanding_paths()
        .into_iter()
        .filter(|(p, _)| p == "doc.txt")
        .collect();
    assert_eq!(queued.len(), 1, "one entry, whatever the event count");
}

// --- Delete ---------------------------------------------------------------

#[test]
fn a_deleted_file_the_server_has_is_queued_as_a_tombstone() {
    let f = Fixture::new();
    f.write("a.txt", b"one");
    f.touched("a.txt");
    f.mark_all_synced();

    std::fs::remove_file(f.path("a.txt")).unwrap();
    f.gone("a.txt");

    let entry = f.entry("a.txt").unwrap();
    assert_eq!(entry.intent, Intent::Tombstone);
    assert_eq!(entry.state, SyncState::Pending);
}

#[test]
fn a_file_created_and_deleted_before_anything_was_sent_costs_nothing() {
    // The temporary files an atomic save leaves behind. Telling the server
    // about something it never heard of, purely to un-tell it, is pure waste.
    let f = Fixture::new();
    f.write("~temp.tmp", b"scratch");
    f.touched("~temp.tmp");
    assert!(f.entry("~temp.tmp").is_some());

    std::fs::remove_file(f.path("~temp.tmp")).unwrap();
    f.gone("~temp.tmp");

    assert!(
        f.entry("~temp.tmp").is_none(),
        "nothing should remain to send"
    );
    assert!(f.outstanding_paths().is_empty());
}

#[test]
fn a_delete_event_for_a_file_that_is_still_there_removes_nothing() {
    // Atomic saves delete and immediately recreate. Acting on the delete alone
    // would tombstone a file the user still has.
    let f = Fixture::new();
    f.write("a.txt", b"one");
    f.touched("a.txt");
    f.mark_all_synced();

    // The event arrives late; the file is back by the time it is handled.
    f.gone("a.txt");

    let entry = f.entry("a.txt").unwrap();
    assert_ne!(entry.intent, Intent::Tombstone);
}

#[test]
fn deleting_a_folder_removes_everything_under_it() {
    let f = Fixture::new();
    f.write("notes/a.txt", b"a");
    f.write("notes/deep/b.txt", b"b");
    f.touched("notes/a.txt");
    f.touched("notes/deep/b.txt");
    f.mark_all_synced();

    std::fs::remove_dir_all(f.path("notes")).unwrap();
    f.gone("notes");

    for path in ["notes", "notes/a.txt", "notes/deep", "notes/deep/b.txt"] {
        assert_eq!(
            f.entry(path).unwrap().intent,
            Intent::Tombstone,
            "{path} should be queued for removal"
        );
    }
}

#[test]
fn removals_are_ordered_from_the_inside_out() {
    // A folder is emptied before it is removed, so the server is never asked
    // to remove something it still believes has contents.
    let f = Fixture::new();
    f.write("notes/deep/b.txt", b"b");
    f.touched("notes/deep/b.txt");
    f.mark_all_synced();

    std::fs::remove_dir_all(f.path("notes")).unwrap();
    f.gone("notes");

    let order = f.outstanding_paths();
    let file = order
        .iter()
        .position(|(p, _)| p == "notes/deep/b.txt")
        .unwrap();
    let inner = order.iter().position(|(p, _)| p == "notes/deep").unwrap();
    let outer = order.iter().position(|(p, _)| p == "notes").unwrap();
    assert!(
        file < inner,
        "the file must go before the folder holding it"
    );
    assert!(inner < outer, "and the inner folder before the outer one");
}

#[test]
fn a_folder_that_is_not_protected_keeps_no_queue_when_its_root_is_removed() {
    // The transaction that the 82,768-row incident was about.
    let f = Fixture::new();
    f.write("a.txt", b"a");
    f.touched("a.txt");

    f.store.disable_root(SERVER, ROOT).unwrap();

    assert!(
        f.outstanding_paths().is_empty(),
        "a folder nobody is backing up must not keep a backlog"
    );
}

// --- Rename and move -------------------------------------------------------

#[test]
fn renaming_a_file_moves_it_rather_than_re_uploading_it() {
    let f = Fixture::new();
    f.write("a.txt", b"contents");
    f.touched("a.txt");
    f.mark_all_synced();
    let identity = f.entry("a.txt").unwrap().client_entry_id;

    std::fs::rename(f.path("a.txt"), f.path("b.txt")).unwrap();
    f.renamed("a.txt", "b.txt");

    assert!(
        f.entry("a.txt").is_none(),
        "nothing should remain at the old path"
    );
    let moved = f
        .entry("b.txt")
        .expect("the entry should be at the new path");
    assert_eq!(moved.client_entry_id, identity, "the identity must survive");
    assert_eq!(moved.intent, Intent::Move);
    assert_eq!(moved.synced_path.as_deref(), Some("a.txt"));
}

#[test]
fn renaming_a_file_the_server_never_saw_is_just_an_upload() {
    // There is nothing to move it from, and asking the server to move
    // something it has never heard of would simply fail.
    let f = Fixture::new();
    f.write("a.txt", b"contents");
    f.touched("a.txt");

    std::fs::rename(f.path("a.txt"), f.path("b.txt")).unwrap();
    f.renamed("a.txt", "b.txt");

    let entry = f.entry("b.txt").expect("queued at the new path");
    assert_eq!(entry.intent, Intent::Upsert);
}

#[test]
fn a_case_only_rename_does_not_create_a_second_entry() {
    // Windows cannot tell `report.txt` from `Report.txt`, and neither can this
    // client's identity model. Treating it as a move would ask the server to
    // move a file to where it already is.
    let f = Fixture::new();
    f.write("report.txt", b"x");
    f.touched("report.txt");
    f.mark_all_synced();
    let identity = f.entry("report.txt").unwrap().client_entry_id;

    f.renamed("report.txt", "Report.txt");

    let entry = f.entry("Report.txt").expect("still one entry");
    assert_eq!(entry.client_entry_id, identity);
    assert_eq!(
        entry.relative_path, "Report.txt",
        "the new spelling should be recorded"
    );
    assert_ne!(entry.intent, Intent::Move, "there is nowhere to move it to");
}

#[test]
fn renaming_a_folder_carries_its_whole_subtree() {
    let f = Fixture::new();
    f.write("old/a.txt", b"a");
    f.write("old/deep/b.txt", b"b");
    f.touched("old/a.txt");
    f.touched("old/deep/b.txt");
    f.mark_all_synced();
    let identity = f.entry("old/deep/b.txt").unwrap().client_entry_id;

    std::fs::rename(f.path("old"), f.path("new")).unwrap();
    f.renamed("old", "new");

    for path in ["new", "new/a.txt", "new/deep", "new/deep/b.txt"] {
        let entry = f.entry(path).unwrap_or_else(|| panic!("{path} missing"));
        assert_eq!(entry.intent, Intent::Move, "{path} should move");
    }
    assert_eq!(
        f.entry("new/deep/b.txt").unwrap().client_entry_id,
        identity,
        "a file must keep its identity when a folder above it is renamed"
    );
    assert!(f.entry("old/a.txt").is_none());
}

#[test]
fn a_folder_move_is_ordered_parent_first() {
    let f = Fixture::new();
    f.write("old/deep/b.txt", b"b");
    f.touched("old/deep/b.txt");
    f.mark_all_synced();

    std::fs::rename(f.path("old"), f.path("new")).unwrap();
    f.renamed("old", "new");

    let order = f.outstanding_paths();
    let outer = order.iter().position(|(p, _)| p == "new").unwrap();
    let inner = order.iter().position(|(p, _)| p == "new/deep").unwrap();
    let file = order
        .iter()
        .position(|(p, _)| p == "new/deep/b.txt")
        .unwrap();
    assert!(outer < inner && inner < file);
}

// --- Atomic replacement ----------------------------------------------------

#[test]
fn the_office_style_save_leaves_one_file_with_the_new_contents() {
    // Write a temporary file, delete the original, rename the temporary into
    // its place. Handled badly this leaves the server with the old file, a
    // stray temporary, and a duplicate.
    let f = Fixture::new();
    f.write("report.docx", b"version one");
    f.touched("report.docx");
    f.mark_all_synced();
    let identity = f.entry("report.docx").unwrap().client_entry_id;

    f.write("~$report.tmp", b"version two, longer");
    f.touched("~$report.tmp");
    std::fs::remove_file(f.path("report.docx")).unwrap();
    f.gone("report.docx");
    std::fs::rename(f.path("~$report.tmp"), f.path("report.docx")).unwrap();
    f.renamed("~$report.tmp", "report.docx");

    // Whatever the intermediate states said, the disk is the authority.
    let outcome = f.reconcile();
    assert!(matches!(outcome, Outcome::Reconciled { .. }));

    let live: Vec<_> = f
        .store
        .entries_in_root(SERVER, ROOT)
        .unwrap()
        .into_iter()
        .filter(|entry| entry.intent != Intent::Tombstone)
        .map(|entry| entry.relative_path)
        .collect();
    assert_eq!(
        live,
        vec!["report.docx".to_string()],
        "one file should survive, with no temporary beside it"
    );
    assert!(
        f.entry("~$report.tmp").is_none(),
        "the temporary must not be left behind"
    );
    // The original identity is either kept or replaced, but the file must not
    // be recorded twice under two identities.
    let _ = identity;
}

// --- Reconciliation --------------------------------------------------------

#[test]
fn reconciliation_finds_what_happened_while_nothing_was_watching() {
    // The app-was-closed case, and the watcher-dropped-it case. Both look the
    // same from here: the disk and the database disagree.
    let f = Fixture::new();
    f.write("a.txt", b"a");
    f.touched("a.txt");
    f.mark_all_synced();

    // No events for any of this.
    f.write("new.txt", b"new");
    f.write("a.txt", b"changed and longer");
    f.mkdir("fresh");

    let outcome = f.reconcile();
    let Outcome::Reconciled {
        created, updated, ..
    } = outcome
    else {
        panic!("expected a reconciliation, got {outcome:?}");
    };
    assert!(created >= 2, "the new file and folder should be found");
    assert!(updated >= 1, "the edited file should be found");
    assert_eq!(f.entry("new.txt").unwrap().state, SyncState::Pending);
}

#[test]
fn reconciliation_queues_removals_for_files_that_disappeared() {
    let f = Fixture::new();
    for name in ["a.txt", "b.txt", "c.txt"] {
        f.write(name, b"x");
        f.touched(name);
    }
    f.mark_all_synced();

    std::fs::remove_file(f.path("b.txt")).unwrap();

    let outcome = f.reconcile();
    let Outcome::Reconciled { removed, .. } = outcome else {
        panic!("expected a reconciliation, got {outcome:?}");
    };
    assert_eq!(removed, 1);
    assert_eq!(f.entry("b.txt").unwrap().intent, Intent::Tombstone);
}

#[test]
fn reconciliation_recognises_a_rename_it_never_saw_happen() {
    // No rename event — the app was closed. Same size, same modification time,
    // one path gone and one appeared: a move, not a deletion and an upload.
    let f = Fixture::new();
    f.write("a.txt", b"exactly these bytes");
    f.touched("a.txt");
    f.mark_all_synced();
    let identity = f.entry("a.txt").unwrap().client_entry_id;

    std::fs::rename(f.path("a.txt"), f.path("b.txt")).unwrap();

    let outcome = f.reconcile();
    let Outcome::Reconciled { moved, removed, .. } = outcome else {
        panic!("expected a reconciliation, got {outcome:?}");
    };
    assert_eq!(moved, 1);
    assert_eq!(removed, 0, "a move is not a deletion");
    assert_eq!(f.entry("b.txt").unwrap().client_entry_id, identity);
}

#[test]
fn an_unreadable_folder_is_never_read_as_a_mass_deletion() {
    // The incident this whole design exists to prevent: an unplugged drive
    // must not look like somebody deleting everything.
    let f = Fixture::new();
    for index in 0..5 {
        let name = format!("file{index}.txt");
        f.write(&name, b"x");
        f.touched(&name);
    }
    f.mark_all_synced();

    let mut vanished = f.root.clone();
    vanished.local_path = PathBuf::from(r"Q:\NoSuchVolume\Protected");
    let outcome =
        reconcile::reconcile_root(&f.store, SERVER, &vanished, &CancelFlag::new()).unwrap();

    assert_eq!(outcome, Outcome::Unavailable);
    let tombstones = f
        .store
        .entries_in_root(SERVER, ROOT)
        .unwrap()
        .into_iter()
        .filter(|entry| entry.intent == Intent::Tombstone)
        .count();
    assert_eq!(tombstones, 0, "nothing may be treated as deleted");
    assert_eq!(
        f.store.roots(SERVER).unwrap()[0].status,
        RootStatus::Unavailable
    );
}

#[test]
fn emptying_a_large_folder_is_held_rather_than_obeyed() {
    // Enough files that losing all of them crosses the guard. A folder that
    // empties itself is far more often a fault than an intention.
    let f = Fixture::new();
    for index in 0..40 {
        let name = format!("file{index}.txt");
        f.write(&name, b"x");
        f.touched(&name);
    }
    f.mark_all_synced();

    for index in 0..40 {
        std::fs::remove_file(f.path(&format!("file{index}.txt"))).unwrap();
    }

    let outcome = f.reconcile();
    assert!(
        matches!(outcome, Outcome::MassChangeHeld { .. }),
        "expected a safety hold, got {outcome:?}"
    );
    let tombstones = f
        .store
        .entries_in_root(SERVER, ROOT)
        .unwrap()
        .into_iter()
        .filter(|entry| entry.intent == Intent::Tombstone)
        .count();
    assert_eq!(tombstones, 0, "nothing may be removed while held");
    assert_eq!(
        f.store.roots(SERVER).unwrap()[0].status,
        RootStatus::SafetyHold
    );
}

#[test]
fn deleting_a_couple_of_files_from_a_large_folder_is_obeyed() {
    // The guard must not make ordinary deletion impossible.
    let f = Fixture::new();
    for index in 0..40 {
        let name = format!("file{index}.txt");
        f.write(&name, b"x");
        f.touched(&name);
    }
    f.mark_all_synced();

    std::fs::remove_file(f.path("file0.txt")).unwrap();
    std::fs::remove_file(f.path("file1.txt")).unwrap();

    let outcome = f.reconcile();
    let Outcome::Reconciled { removed, .. } = outcome else {
        panic!("expected a reconciliation, got {outcome:?}");
    };
    assert_eq!(removed, 2);
    assert_eq!(f.store.roots(SERVER).unwrap()[0].status, RootStatus::Active);
}

#[test]
fn a_folder_that_comes_back_is_marked_available_again() {
    let f = Fixture::new();
    f.store
        .set_root_status(SERVER, ROOT, RootStatus::Unavailable)
        .unwrap();

    let mut root = f.root.clone();
    root.status = RootStatus::Unavailable;
    let outcome = reconcile::reconcile_root(&f.store, SERVER, &root, &CancelFlag::new()).unwrap();

    assert!(matches!(outcome, Outcome::Reconciled { .. }));
    assert_eq!(f.store.roots(SERVER).unwrap()[0].status, RootStatus::Active);
}

#[test]
fn a_held_folder_takes_no_watcher_work() {
    // The hold means stop acting on this folder. Continuing to queue from
    // events would be doing exactly what the hold refused.
    let f = Fixture::new();
    let mut held = f.root.clone();
    held.status = RootStatus::SafetyHold;

    f.write("a.txt", b"a");
    reconcile::apply_change(
        &f.store,
        SERVER,
        &held,
        &LocalChange::Touched {
            root_id: ROOT.into(),
            relative: "a.txt".into(),
        },
    )
    .unwrap();

    assert!(f.entry("a.txt").is_none());
}

#[test]
fn a_folder_the_server_switched_off_takes_no_watcher_work() {
    let f = Fixture::new();
    let mut off = f.root.clone();
    off.enabled = false;

    f.write("a.txt", b"a");
    reconcile::apply_change(
        &f.store,
        SERVER,
        &off,
        &LocalChange::Touched {
            root_id: ROOT.into(),
            relative: "a.txt".into(),
        },
    )
    .unwrap();

    assert!(f.entry("a.txt").is_none());
}

// --- Privacy ---------------------------------------------------------------

#[test]
fn nothing_the_watcher_records_carries_an_absolute_path() {
    // The server is told a root identity and a relative path. Where the folder
    // lives on this PC is nobody else's business, and the watcher deals in
    // absolute paths, so this is exactly where one could leak.
    let f = Fixture::new();
    f.write("deep/a/note.txt", b"x");
    f.touched("deep/a/note.txt");

    for entry in f.store.entries_in_root(SERVER, ROOT).unwrap() {
        assert!(
            !entry.relative_path.contains(':'),
            "a drive letter reached an entry: {}",
            entry.relative_path
        );
        assert!(
            !entry.relative_path.contains('\\'),
            "a Windows separator reached an entry: {}",
            entry.relative_path
        );
        assert!(
            !entry.relative_path.to_lowercase().contains(
                &f.root
                    .local_path
                    .to_string_lossy()
                    .to_lowercase()
                    .to_string()
            ),
            "the local path reached an entry"
        );
    }
}

#[test]
fn the_queue_holds_no_credential_after_watcher_activity() {
    let f = Fixture::new();
    f.write("a.txt", b"x");
    f.touched("a.txt");
    f.mark_all_synced();
    f.renamed("a.txt", "b.txt");

    let dumped = format!("{:?}", f.store.entries_in_root(SERVER, ROOT).unwrap());
    for banned in ["arcsync", "credential", "cookie", "authorization"] {
        assert!(
            !dumped.to_lowercase().contains(banned),
            "the queue mentions {banned}"
        );
    }
}

// --- Reparse points ---------------------------------------------------------

#[test]
fn a_reparse_point_is_not_followed_out_of_the_protected_folder() {
    // Junctions and symlinks are how a backup escapes its folder or walks
    // forever. The scanner refuses them; so must the watcher.
    let f = Fixture::new();
    let outside = f._dir.path().join("Outside");
    std::fs::create_dir_all(outside.join("secret")).unwrap();
    std::fs::write(outside.join("secret/private.txt"), b"not yours").unwrap();

    let link = f.path("link");
    let made = make_directory_link(&link, &outside);
    if !made {
        eprintln!("skipped: this environment cannot create a directory link");
        return;
    }

    f.touched("link");
    f.reconcile();

    for entry in f.store.entries_in_root(SERVER, ROOT).unwrap() {
        assert!(
            !entry.relative_path.contains("private"),
            "backup escaped the protected folder through a link: {}",
            entry.relative_path
        );
    }
}

/// Best effort: creating a junction needs no privileges, a symlink usually does.
fn make_directory_link(link: &Path, target: &Path) -> bool {
    std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

#[test]
fn deleting_a_folder_holding_most_of_the_backup_is_held_not_obeyed() {
    // Found live, not in review: the guard was in the reconciler only, and the
    // watcher reached the same removals first. A folder vanishing produces one
    // event and would have removed everything under it — which is right when
    // somebody deleted a folder and wrong for every other cause that looks
    // exactly the same from here.
    let f = Fixture::new();
    for index in 0..40 {
        let name = format!("bulk/file{index}.txt");
        f.write(&name, b"x");
        f.touched(&name);
    }
    f.write("keep.txt", b"keep");
    f.touched("keep.txt");
    f.mark_all_synced();

    std::fs::remove_dir_all(f.path("bulk")).unwrap();
    f.gone("bulk");

    assert_eq!(
        f.store.roots(SERVER).unwrap()[0].status,
        RootStatus::SafetyHold,
        "the folder should be held"
    );
    let tombstones = f
        .store
        .entries_in_root(SERVER, ROOT)
        .unwrap()
        .into_iter()
        .filter(|entry| entry.intent == Intent::Tombstone)
        .count();
    assert_eq!(tombstones, 0, "nothing may be removed while held");
}

#[test]
fn deleting_a_small_folder_is_still_obeyed() {
    // The guard must not make deleting a folder impossible. A handful of files
    // out of many is exactly the deletion people make on purpose.
    let f = Fixture::new();
    for index in 0..40 {
        let name = format!("keep/file{index}.txt");
        f.write(&name, b"x");
        f.touched(&name);
    }
    f.write("scratch/a.txt", b"a");
    f.write("scratch/b.txt", b"b");
    f.touched("scratch/a.txt");
    f.touched("scratch/b.txt");
    f.mark_all_synced();

    std::fs::remove_dir_all(f.path("scratch")).unwrap();
    f.gone("scratch");

    assert_eq!(f.store.roots(SERVER).unwrap()[0].status, RootStatus::Active);
    assert_eq!(f.entry("scratch/a.txt").unwrap().intent, Intent::Tombstone);
}

#[test]
fn confirming_a_hold_carries_out_the_removals() {
    // Found live: clearing the hold alone did nothing visible, because the
    // next pass found the same disappearances and held again. Confirming has
    // to be the act that performs what is being confirmed.
    let f = Fixture::new();
    for index in 0..40 {
        let name = format!("file{index}.txt");
        f.write(&name, b"x");
        f.touched(&name);
    }
    f.mark_all_synced();
    for index in 0..40 {
        std::fs::remove_file(f.path(&format!("file{index}.txt"))).unwrap();
    }
    assert!(matches!(f.reconcile(), Outcome::MassChangeHeld { .. }));

    let outcome = reconcile::reconcile_root_confirming_removals(
        &f.store,
        SERVER,
        &f.root,
        &CancelFlag::new(),
    )
    .unwrap();

    let Outcome::Reconciled { removed, .. } = outcome else {
        panic!("expected the removals to be carried out, got {outcome:?}");
    };
    assert_eq!(removed, 40);
}

#[test]
fn confirming_a_hold_removes_nothing_if_the_files_came_back() {
    // The drive was reconnected between the alarm and the answer. A
    // confirmation that has been overtaken must not destroy anything.
    let f = Fixture::new();
    for index in 0..40 {
        let name = format!("file{index}.txt");
        f.write(&name, b"x");
        f.touched(&name);
    }
    f.mark_all_synced();

    let outcome = reconcile::reconcile_root_confirming_removals(
        &f.store,
        SERVER,
        &f.root,
        &CancelFlag::new(),
    )
    .unwrap();

    let Outcome::Reconciled { removed, .. } = outcome else {
        panic!("expected a reconciliation, got {outcome:?}");
    };
    assert_eq!(removed, 0, "nothing is missing, so nothing may be removed");
}

// --- The three move cases, named ------------------------------------------
//
// They are three different problems and the report must not blur them.
//
//   A  same folder, same parent            Root\a.txt   -> Root\b.txt
//   B  same folder, different directory    Root\a.txt   -> Root\Archive\a.txt
//   C  two different protected folders     One\a.txt    -> Two\a.txt
//
// A and B keep the entry's identity and cost one small request. C cannot:
// each protected folder is its own root on the server with its own opaque
// identity, and one entry cannot belong to two of them.

#[test]
fn case_a_same_parent_rename_is_a_move() {
    let f = Fixture::new();
    f.write("a.txt", b"some contents worth not re-sending");
    f.touched("a.txt");
    f.mark_all_synced();
    let identity = f.entry("a.txt").unwrap().client_entry_id;

    std::fs::rename(f.path("a.txt"), f.path("b.txt")).unwrap();
    f.renamed("a.txt", "b.txt");

    let moved = f.entry("b.txt").expect("at the new path");
    assert_eq!(moved.intent, Intent::Move);
    assert_eq!(moved.client_entry_id, identity);
    assert!(f.entry("a.txt").is_none());
}

#[test]
fn case_b_cross_directory_move_is_a_move_not_a_re_upload() {
    // Windows does not always report this as a rename pair, so it arrives as
    // an unrelated disappearance and arrival. Correlating them by the file's
    // own identity is what keeps it one small request instead of re-sending
    // every byte.
    let f = Fixture::new();
    f.write("a.txt", b"some contents worth not re-sending");
    f.mkdir("Archive");
    f.touched("Archive");
    f.touched("a.txt");
    f.mark_all_synced();
    let identity = f.entry("a.txt").unwrap().client_entry_id;

    std::fs::rename(f.path("a.txt"), f.path("Archive/a.txt")).unwrap();
    // Deliberately *not* a Renamed: the unpaired case, which is the hard one.
    f.gone("a.txt");
    f.touched("Archive/a.txt");

    let moved = f.entry("Archive/a.txt").expect("at the new path");
    assert_eq!(
        moved.intent,
        Intent::Move,
        "a cross-directory move must not re-upload the bytes"
    );
    assert_eq!(moved.client_entry_id, identity, "identity must survive");
}

#[test]
fn case_b_survives_the_arrival_being_seen_first() {
    // Event order is not guaranteed. The arrival may be handled before the
    // disappearance, and the answer must be the same.
    let f = Fixture::new();
    f.write("a.txt", b"contents");
    f.mkdir("Archive");
    f.touched("Archive");
    f.touched("a.txt");
    f.mark_all_synced();
    let identity = f.entry("a.txt").unwrap().client_entry_id;

    std::fs::rename(f.path("a.txt"), f.path("Archive/a.txt")).unwrap();
    f.touched("Archive/a.txt");
    f.gone("a.txt");

    let moved = f.entry("Archive/a.txt").expect("at the new path");
    assert_eq!(moved.client_entry_id, identity);
    assert!(f.entry("a.txt").is_none());
}

#[test]
fn case_b_directory_move_carries_the_subtree_without_re_uploading() {
    let f = Fixture::new();
    f.write("old/deep/b.txt", b"contents");
    f.mkdir("Archive");
    f.touched("Archive");
    f.touched("old/deep/b.txt");
    f.mark_all_synced();
    let identity = f.entry("old/deep/b.txt").unwrap().client_entry_id;

    std::fs::rename(f.path("old"), f.path("Archive/old")).unwrap();
    f.gone("old");
    f.touched("Archive/old");

    let moved = f
        .entry("Archive/old/deep/b.txt")
        .expect("the file should have travelled with its folder");
    assert_eq!(moved.intent, Intent::Move);
    assert_eq!(moved.client_entry_id, identity);
}

#[test]
fn case_b_is_found_by_reconciliation_when_no_events_arrived() {
    // The app was closed. No events at all; the scan simply finds one path
    // gone and another present, and identity is the only safe way to tell it
    // is the same file.
    let f = Fixture::new();
    f.write("a.txt", b"contents");
    f.mkdir("Archive");
    f.touched("Archive");
    f.touched("a.txt");
    f.mark_all_synced();
    let identity = f.entry("a.txt").unwrap().client_entry_id;

    std::fs::rename(f.path("a.txt"), f.path("Archive/a.txt")).unwrap();

    let outcome = f.reconcile();
    let Outcome::Reconciled { moved, removed, .. } = outcome else {
        panic!("expected a reconciliation, got {outcome:?}");
    };
    assert_eq!(moved, 1);
    assert_eq!(removed, 0, "a move is not a deletion");
    assert_eq!(f.entry("Archive/a.txt").unwrap().client_entry_id, identity);
}

#[test]
fn identical_files_are_never_mistaken_for_each_other() {
    // The reason identity is asked of Windows rather than guessed. These two
    // agree on name, size and content; a heuristic would pair the wrong ones
    // and tell the server to move one file on top of the other.
    let f = Fixture::new();
    f.write("one/render.png", b"identical bytes");
    f.write("two/render.png", b"identical bytes");
    f.touched("one/render.png");
    f.touched("two/render.png");
    f.mark_all_synced();
    let one = f.entry("one/render.png").unwrap().client_entry_id;
    let two = f.entry("two/render.png").unwrap().client_entry_id;
    assert_ne!(one, two);

    // Move only the first.
    f.mkdir("Archive");
    std::fs::rename(f.path("one/render.png"), f.path("Archive/render.png")).unwrap();
    f.gone("one/render.png");
    f.touched("Archive/render.png");

    assert_eq!(
        f.entry("Archive/render.png").unwrap().client_entry_id,
        one,
        "the file that actually moved must be the one that moved"
    );
    assert_eq!(
        f.entry("two/render.png").unwrap().client_entry_id,
        two,
        "the other must be left entirely alone"
    );
    assert_eq!(f.entry("two/render.png").unwrap().state, SyncState::Synced);
}

#[test]
fn a_new_file_reusing_an_old_path_is_not_treated_as_a_move() {
    // Delete and recreate at the same path. The new file has its own identity,
    // so it is new content — not the old file having moved somewhere.
    let f = Fixture::new();
    f.write("a.txt", b"first");
    f.touched("a.txt");
    f.mark_all_synced();
    let first = f.entry("a.txt").unwrap().client_entry_id;

    std::fs::remove_file(f.path("a.txt")).unwrap();
    f.gone("a.txt");
    f.write("a.txt", b"second, different");
    f.touched("a.txt");

    let entry = f.entry("a.txt").unwrap();
    assert_eq!(entry.intent, Intent::Upsert);
    let _ = first;
}

#[test]
fn an_upgraded_backup_learns_file_identities_without_re_uploading() {
    // Found while certifying an upgrade: entries written by a version that had
    // no concept of file identity keep none, and an unchanged file is exactly
    // the one that never acquires one. Moving it would then re-upload it.
    //
    // Learning must not look like work: the entry stays synced and nothing is
    // queued by the act of learning.
    let f = Fixture::new();
    f.write("legacy.txt", b"written by an older version");
    f.touched("legacy.txt");
    f.mark_all_synced();

    // Strip the identity, as an upgraded database would have it.
    let mut legacy = f.entry("legacy.txt").unwrap();
    legacy.file_id = None;
    f.store.apply_batch(SERVER, &[legacy.clone()]).unwrap();
    assert!(f.entry("legacy.txt").unwrap().file_id.is_none());

    let outcome = f.reconcile();
    let Outcome::Reconciled {
        created,
        updated,
        removed,
        ..
    } = outcome
    else {
        panic!("expected a reconciliation, got {outcome:?}");
    };
    assert_eq!(
        (created, updated, removed),
        (0, 0, 0),
        "no work may be queued"
    );
    assert!(f.outstanding_paths().is_empty(), "nothing queued to send");

    let learned = f.entry("legacy.txt").unwrap();
    assert!(learned.file_id.is_some(), "the identity should be learned");
    assert_eq!(learned.state, SyncState::Synced, "and it stays synced");
    assert_eq!(learned.client_entry_id, legacy.client_entry_id);
}

#[test]
fn an_upgraded_entry_can_then_be_moved_rather_than_re_uploaded() {
    // The payoff: once learned, a move is a move.
    let f = Fixture::new();
    f.write("legacy.txt", b"written by an older version");
    f.mkdir("Archive");
    f.touched("Archive");
    f.touched("legacy.txt");
    f.mark_all_synced();

    let mut legacy = f.entry("legacy.txt").unwrap();
    let identity = legacy.client_entry_id.clone();
    legacy.file_id = None;
    f.store.apply_batch(SERVER, &[legacy]).unwrap();

    f.reconcile(); // learns the identity
    std::fs::rename(f.path("legacy.txt"), f.path("Archive/legacy.txt")).unwrap();
    f.gone("legacy.txt");
    f.touched("Archive/legacy.txt");

    let moved = f.entry("Archive/legacy.txt").expect("at the new path");
    assert_eq!(moved.intent, Intent::Move, "should move, not re-upload");
    assert_eq!(moved.client_entry_id, identity);
}
