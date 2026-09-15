//! What happens after the event stream had a hole in it.
//!
//! Waking from sleep is the obvious cause; a stalled disk, a starved process
//! or a watcher error produce the same situation. Notifications were missed,
//! and the handles meant to deliver them may be stale.
//!
//! Detecting the gap proves nothing on its own. These assert on what follows:
//! the folders are read again, the watcher is registered against what
//! qualifies now, nothing is registered twice, and whatever changed while
//! nobody was looking is found.

use std::path::PathBuf;
use std::sync::Arc;

use arciin_desktop_lib::backup::store::{Intent, Profile, Root, RootStatus, SyncState, SyncStore};
use arciin_desktop_lib::backup::supervisor::{resume_after_gap, Pending, Resumed};
use arciin_desktop_lib::backup::watcher::WatcherManager;

const SERVER: &str = "3f2504e0-4f89-41d3-9a0c-0305e82c3301";

struct Harness {
    _dir: tempfile::TempDir,
    store: Arc<SyncStore>,
    watcher: WatcherManager,
    pending: Arc<Pending>,
    protected: PathBuf,
}

impl Harness {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let protected = dir.path().join("Protected");
        std::fs::create_dir_all(&protected).unwrap();
        let store = Arc::new(SyncStore::open(&dir.path().join("state")).unwrap());
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
        store
            .save_root(
                SERVER,
                &Root {
                    id: "root-1".into(),
                    kind: "CUSTOM".into(),
                    display_name: "Protected".into(),
                    local_path: protected.clone(),
                    enabled: true,
                    status: RootStatus::Active,
                },
            )
            .unwrap();
        Self {
            _dir: dir,
            store,
            watcher: WatcherManager::new(),
            pending: Arc::new(Pending::default()),
            protected,
        }
    }

    fn resume(&self) -> Resumed {
        resume_after_gap(&self.store, SERVER, &self.watcher, &self.pending).unwrap()
    }

    fn write(&self, name: &str, contents: &[u8]) {
        let path = self.protected.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, contents).unwrap();
    }

    fn mark_all_synced(&self) {
        for entry in self.store.outstanding(SERVER, 10_000).unwrap() {
            self.store
                .complete_operation(SERVER, &entry.client_entry_id, SyncState::Synced)
                .unwrap();
        }
    }
}

#[test]
fn resuming_registers_the_watcher_for_every_protected_folder() {
    let h = Harness::new();
    assert!(
        h.watcher.registered().is_empty(),
        "nothing before the resume"
    );

    let summary = h.resume();

    assert_eq!(summary.watchable, 1);
    assert_eq!(summary.registered, 1);
    assert_eq!(h.watcher.registered(), vec!["root-1".to_string()]);
}

#[test]
fn resuming_twice_does_not_watch_the_same_folder_twice() {
    // Windows may invalidate handles across a suspend, so a resume has to be
    // free to re-register. If that doubled the registrations, every later
    // change would be queued twice — and on a real folder, uploaded twice.
    let h = Harness::new();
    h.resume();
    h.resume();
    let summary = h.resume();

    assert_eq!(summary.registered, 1);
    assert_eq!(h.watcher.registered().len(), 1);
}

#[test]
fn resuming_finds_what_changed_while_nobody_was_watching() {
    // The correctness property. During a suspend no events are delivered at
    // all, so the resume has to discover the changes by reading.
    let h = Harness::new();
    h.write("before.txt", b"before");
    h.resume();
    h.mark_all_synced();

    // The gap: files change with nothing listening.
    h.write("during-the-gap.txt", b"appeared while suspended");
    h.write("before.txt", b"edited while suspended, and longer");

    h.resume();

    let created = h
        .store
        .entry_by_path(SERVER, "root-1", "during-the-gap.txt")
        .unwrap()
        .expect("the new file must be found");
    assert_eq!(created.state, SyncState::Pending);

    let edited = h
        .store
        .entry_by_path(SERVER, "root-1", "before.txt")
        .unwrap()
        .unwrap();
    assert_eq!(edited.state, SyncState::Pending, "the edit must be found");
}

#[test]
fn resuming_finds_a_deletion_that_happened_during_the_gap() {
    let h = Harness::new();
    h.write("a.txt", b"a");
    h.write("b.txt", b"b");
    h.resume();
    h.mark_all_synced();

    std::fs::remove_file(h.protected.join("b.txt")).unwrap();
    h.resume();

    assert_eq!(
        h.store
            .entry_by_path(SERVER, "root-1", "b.txt")
            .unwrap()
            .unwrap()
            .intent,
        Intent::Tombstone
    );
}

#[test]
fn resuming_finds_a_move_that_happened_during_the_gap() {
    // No events at all, so identity is the only thing that can tell this from
    // a deletion plus an unrelated new file.
    let h = Harness::new();
    h.write("a.txt", b"contents");
    std::fs::create_dir_all(h.protected.join("Archive")).unwrap();
    h.resume();
    h.mark_all_synced();
    let identity = h
        .store
        .entry_by_path(SERVER, "root-1", "a.txt")
        .unwrap()
        .unwrap()
        .client_entry_id;

    std::fs::rename(
        h.protected.join("a.txt"),
        h.protected.join("Archive").join("a.txt"),
    )
    .unwrap();
    h.resume();

    let moved = h
        .store
        .entry_by_path(SERVER, "root-1", "Archive/a.txt")
        .unwrap()
        .expect("found at the new path");
    assert_eq!(moved.client_entry_id, identity);
    assert_eq!(moved.intent, Intent::Move, "not a re-upload");
}

#[test]
fn a_folder_that_went_away_during_the_gap_is_unavailable_not_emptied() {
    // The drive was unplugged while the machine slept. This must never read as
    // the user having deleted everything in it.
    let h = Harness::new();
    for index in 0..30 {
        h.write(&format!("file{index}.txt"), b"x");
    }
    h.resume();
    h.mark_all_synced();

    std::fs::remove_dir_all(&h.protected).unwrap();
    let summary = h.resume();

    assert_eq!(summary.unavailable, 1);
    assert_eq!(summary.watchable, 0, "an unreadable folder is not watched");
    assert_eq!(summary.registered, 0, "and holds no registration");
    let tombstones = h
        .store
        .entries_in_root(SERVER, "root-1")
        .unwrap()
        .into_iter()
        .filter(|entry| entry.intent == Intent::Tombstone)
        .count();
    assert_eq!(tombstones, 0, "nothing may be treated as deleted");
    assert_eq!(
        h.store.roots(SERVER).unwrap()[0].status,
        RootStatus::Unavailable
    );
}

#[test]
fn a_folder_that_comes_back_is_watched_and_reconciled_again() {
    let h = Harness::new();
    h.write("a.txt", b"a");
    h.resume();
    h.mark_all_synced();

    let stashed = h.protected.with_file_name("Stashed");
    std::fs::rename(&h.protected, &stashed).unwrap();
    let away = h.resume();
    assert_eq!(away.unavailable, 1);
    assert_eq!(away.registered, 0);

    std::fs::rename(&stashed, &h.protected).unwrap();
    std::fs::write(h.protected.join("returned.txt"), b"new while away").unwrap();
    let back = h.resume();

    assert_eq!(back.unavailable, 0);
    assert_eq!(back.watchable, 1);
    assert_eq!(back.registered, 1, "watched again, exactly once");
    assert_eq!(h.store.roots(SERVER).unwrap()[0].status, RootStatus::Active);
    assert!(
        h.store
            .entry_by_path(SERVER, "root-1", "returned.txt")
            .unwrap()
            .is_some(),
        "and what changed while it was away is found"
    );
}

#[test]
fn a_folder_the_server_switched_off_is_not_re_watched_by_a_resume() {
    let h = Harness::new();
    h.resume();
    h.store.disable_root(SERVER, "root-1").unwrap();

    let summary = h.resume();

    assert_eq!(summary.watchable, 0);
    assert_eq!(summary.registered, 0, "the registration must be dropped");
}

#[test]
fn a_held_folder_stays_held_across_a_resume() {
    // A resume must not quietly undo a safety hold; it is waiting on a person,
    // not on time passing.
    let h = Harness::new();
    h.write("a.txt", b"a");
    h.resume();
    h.mark_all_synced();
    h.store
        .set_root_status(SERVER, "root-1", RootStatus::SafetyHold)
        .unwrap();

    let summary = h.resume();

    assert_eq!(summary.watchable, 0);
    assert_eq!(
        h.store.roots(SERVER).unwrap()[0].status,
        RootStatus::SafetyHold
    );
}
