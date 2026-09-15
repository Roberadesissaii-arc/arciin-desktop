//! Opening a database written by an older version.
//!
//! Every shipped version leaves databases behind on people's machines, and the
//! next one has to open them. Getting this wrong is not a cosmetic failure: the
//! queue *is* the record of what has been backed up, so a migration that
//! refuses to open, or that loses a column, means either the client stops
//! working or it re-uploads everything it already sent.
//!
//! These build databases in the exact shape older versions wrote and open them
//! with the current code. The shapes are written out longhand rather than
//! generated, because the point is to preserve what those versions actually
//! did, not what the current schema thinks they did.

use arciin_desktop_lib::backup::store::{Intent, RootStatus, SyncState, SyncStore};
use rusqlite::Connection;

const SERVER: &str = "3f2504e0-4f89-41d3-9a0c-0305e82c3301";

/// Version 1: before backup could be switched off, before the watcher.
fn write_v1(path: &std::path::Path) {
    let conn = Connection::open(path.join("backup-state.db")).unwrap();
    conn.execute_batch(
        "CREATE TABLE profiles (
             server_id TEXT PRIMARY KEY, profile_id TEXT NOT NULL,
             device_id TEXT NOT NULL, user_id TEXT NOT NULL,
             paused INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL);
         CREATE TABLE roots (
             id TEXT NOT NULL, server_id TEXT NOT NULL, kind TEXT NOT NULL,
             display_name TEXT NOT NULL, local_path TEXT NOT NULL,
             enabled INTEGER NOT NULL DEFAULT 1,
             PRIMARY KEY (server_id, id));
         CREATE TABLE entries (
             client_entry_id TEXT NOT NULL, server_id TEXT NOT NULL,
             root_id TEXT NOT NULL, relative_path TEXT NOT NULL,
             path_key TEXT NOT NULL, entry_type TEXT NOT NULL,
             size_bytes INTEGER NOT NULL DEFAULT 0,
             modified_ms INTEGER NOT NULL DEFAULT 0,
             state TEXT NOT NULL, pending_operation_id TEXT,
             updated_at TEXT NOT NULL,
             PRIMARY KEY (server_id, client_entry_id));
         CREATE UNIQUE INDEX entries_path ON entries (server_id, root_id, path_key);
         PRAGMA user_version = 1;",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO profiles VALUES (?1,'profile-1','device-1','user-1',0,'2026-01-01T00:00:00Z')",
        [SERVER],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO roots VALUES ('root-1',?1,'CUSTOM','TestBackup','D:\\Profiles\\TestUser\\TestBackup',1)",
        [SERVER],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO entries VALUES ('entry-1',?1,'root-1','a.txt','a.txt','FILE',10,1700000000000,'SYNCED',NULL,'2026-01-01T00:00:00Z')",
        [SERVER],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO entries VALUES ('entry-2',?1,'root-1','b.txt','b.txt','FILE',20,1700000000000,'PENDING',NULL,'2026-01-01T00:00:00Z')",
        [SERVER],
    )
    .unwrap();
}

/// Version 2: backup could be switched off; still no watcher.
fn write_v2(path: &std::path::Path) {
    write_v1(path);
    let conn = Connection::open(path.join("backup-state.db")).unwrap();
    conn.execute_batch(
        "ALTER TABLE profiles ADD COLUMN enabled INTEGER NOT NULL DEFAULT 1;
         PRAGMA user_version = 2;",
    )
    .unwrap();
}

#[test]
fn a_version_one_database_opens_and_keeps_its_work() {
    let dir = tempfile::tempdir().unwrap();
    write_v1(dir.path());

    let store = SyncStore::open(dir.path()).unwrap();

    let profile = store.profile(SERVER).unwrap().expect("profile survived");
    assert_eq!(profile.profile_id, "profile-1");
    assert!(
        profile.enabled,
        "a computer that was backing up before the upgrade still is"
    );

    let roots = store.roots(SERVER).unwrap();
    assert_eq!(roots.len(), 1);
    assert_eq!(
        roots[0].status,
        RootStatus::Active,
        "a folder with no recorded status is available, not held"
    );

    // Both entries survive, and the queued one is still queued.
    let queued = store.outstanding(SERVER, 100).unwrap();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].relative_path, "b.txt");
}

#[test]
fn a_version_two_database_upgrades_all_the_way() {
    // The jump this release actually ships: the last public-facing schema
    // before the watcher, opened by the version with it.
    let dir = tempfile::tempdir().unwrap();
    write_v2(dir.path());

    let store = SyncStore::open(dir.path()).unwrap();

    let entry = store
        .entry_by_path(SERVER, "root-1", "a.txt")
        .unwrap()
        .expect("the synced entry survived");
    assert_eq!(entry.state, SyncState::Synced);
    assert_eq!(
        entry.intent,
        Intent::Upsert,
        "an entry with no recorded intent is an upsert, never a deletion"
    );
    assert_eq!(
        entry.synced_path.as_deref(),
        Some("a.txt"),
        "a synced entry is, by definition, where the server has it — without \
         this every existing file would look unsent and a rename would \
         re-upload it"
    );
    assert!(
        entry.file_id.is_none(),
        "identity is learned when the file is next looked at, not invented"
    );
}

#[test]
fn an_unsent_entry_is_not_claimed_to_be_on_the_server() {
    // The other half of the backfill. Only SYNCED rows get a `synced_path`;
    // claiming one for a pending row would turn its first upload into a move
    // of something the server has never seen.
    let dir = tempfile::tempdir().unwrap();
    write_v2(dir.path());

    let store = SyncStore::open(dir.path()).unwrap();

    let pending = store
        .entry_by_path(SERVER, "root-1", "b.txt")
        .unwrap()
        .unwrap();
    assert_eq!(pending.state, SyncState::Pending);
    assert!(
        pending.synced_path.is_none(),
        "the server has never seen this one"
    );
}

#[test]
fn opening_an_upgraded_database_twice_is_harmless() {
    // Migration runs on every open. Adding a column that is already there is
    // an error, not a no-op, so this is worth asserting rather than assuming.
    let dir = tempfile::tempdir().unwrap();
    write_v2(dir.path());

    for _ in 0..3 {
        let store = SyncStore::open(dir.path()).unwrap();
        assert!(store.profile(SERVER).unwrap().is_some());
    }
}

#[test]
fn a_fresh_database_needs_no_migration() {
    let dir = tempfile::tempdir().unwrap();
    let store = SyncStore::open(dir.path()).unwrap();
    assert!(store.profile(SERVER).unwrap().is_none());
    drop(store);
    // And opening it again still works.
    let store = SyncStore::open(dir.path()).unwrap();
    assert!(store.roots(SERVER).unwrap().is_empty());
}
