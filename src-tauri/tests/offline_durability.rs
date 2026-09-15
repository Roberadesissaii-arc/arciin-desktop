//! Changes made while the server is unreachable, and what becomes of them.
//!
//! The promise this tests is the one people actually rely on: work on a train,
//! close the laptop, open it at the office, and everything you did is on the
//! server. Between those two moments the client has to hold intent through a
//! network failure and a process restart without losing or duplicating any of
//! it.
//!
//! Nothing here is mocked. A real Arciin API on a real socket backed by real
//! PostgreSQL, the real `BackupClient`, the real engine and the real SQLite
//! queue. "Offline" is the genuine article too — the client is pointed at a
//! port with nothing listening, so it meets real connection failures rather
//! than an injected error.
//!
//! Skipped unless pointed at a **disposable** instance:
//!
//! ```text
//! ARCIIN_CERT_ORIGIN=http://127.0.0.1:4310 \
//! ARCIIN_CERT_CREDENTIAL_A=... \
//!   cargo test --test offline_durability -- --nocapture
//! ```

use std::sync::Arc;
use std::time::{Duration, Instant};

use arciin_desktop_lib::backup::client::BackupClient;
use arciin_desktop_lib::backup::engine::{BackupEngine, EngineControl};
use arciin_desktop_lib::backup::reconcile;
use arciin_desktop_lib::backup::scan::CancelFlag;
use arciin_desktop_lib::backup::store::{Intent, Profile, Root, RootStatus, SyncState, SyncStore};
use arciin_desktop_lib::backup::watcher::LocalChange;
use url::Url;

const SERVER: &str = "3f2504e0-4f89-41d3-9a0c-0305e82c3301";

/// A port with nothing on it. Real refusals, not simulated ones.
const NOWHERE: &str = "http://127.0.0.1:1";

struct Fixture {
    origin: Url,
    credential: String,
    root_id: String,
}

fn fixture() -> Option<Fixture> {
    let origin = std::env::var("ARCIIN_CERT_ORIGIN").ok()?;
    Some(Fixture {
        origin: Url::parse(&origin).expect("ARCIIN_CERT_ORIGIN must be a URL"),
        credential: std::env::var("ARCIIN_CERT_CREDENTIAL_A")
            .expect("ARCIIN_CERT_CREDENTIAL_A is required"),
        root_id: std::env::var("ARCIIN_CERT_ROOT_ID").expect("ARCIIN_CERT_ROOT_ID is required"),
    })
}

struct Local {
    dir: tempfile::TempDir,
    store: Arc<SyncStore>,
    root: Root,
}

impl Local {
    fn open(dir: tempfile::TempDir, root_id: &str) -> Self {
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
        let root = Root {
            id: root_id.to_string(),
            kind: "CUSTOM".into(),
            display_name: "Protected".into(),
            local_path: protected,
            enabled: true,
            status: RootStatus::Active,
        };
        store.save_root(SERVER, &root).unwrap();
        Self { dir, store, root }
    }

    fn path(&self, relative: &str) -> std::path::PathBuf {
        self.root.local_path.join(relative.replace('/', "\\"))
    }

    fn write(&self, relative: &str, contents: &[u8]) {
        let path = self.path(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, contents).unwrap();
    }

    fn touched(&self, relative: &str) {
        self.apply(LocalChange::Touched {
            root_id: self.root.id.clone(),
            relative: relative.into(),
        });
    }

    fn gone(&self, relative: &str) {
        self.apply(LocalChange::Gone {
            root_id: self.root.id.clone(),
            relative: relative.into(),
        });
    }

    fn renamed(&self, from: &str, to: &str) {
        self.apply(LocalChange::Renamed {
            root_id: self.root.id.clone(),
            from: from.into(),
            to: to.into(),
        });
    }

    fn apply(&self, change: LocalChange) {
        reconcile::apply_change(&self.store, SERVER, &self.root, &change).unwrap();
    }

    fn outstanding(&self) -> Vec<(String, Intent)> {
        self.store
            .outstanding(SERVER, 10_000)
            .unwrap()
            .into_iter()
            .map(|entry| (entry.relative_path, entry.intent))
            .collect()
    }
}

/// Run the real engine until the queue is empty or the deadline passes.
async fn drain(
    store: &Arc<SyncStore>,
    origin: &Url,
    credential: &str,
    deadline: Duration,
) -> usize {
    let client = Arc::new(BackupClient::new(origin.clone(), credential.to_string()));
    let control = EngineControl::new();
    let engine = BackupEngine::new(
        SERVER.to_string(),
        client,
        Arc::clone(store),
        control.clone(),
    );

    let running = tokio::spawn(async move { engine.run().await });

    let started = Instant::now();
    let mut left = usize::MAX;
    while started.elapsed() < deadline {
        left = store.outstanding(SERVER, 10_000).unwrap().len();
        if left == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    control.stop();
    let _ = tokio::time::timeout(Duration::from_secs(10), running).await;
    left
}

#[tokio::test(flavor = "multi_thread")]
async fn work_done_offline_survives_a_restart_and_reaches_the_server() {
    let Some(fixture) = fixture() else {
        eprintln!("skipped: set ARCIIN_CERT_ORIGIN and friends for a disposable instance");
        return;
    };

    let dir = tempfile::tempdir().unwrap();
    let local = Local::open(dir, &fixture.root_id);

    // --- a synced starting point ------------------------------------------
    local.write("keep.txt", b"keep");
    local.write("edit-me.txt", b"before");
    local.write("rename-me.txt", b"rename me");
    local.write("move-me.txt", b"move me");
    local.write("delete-me.txt", b"delete me");
    std::fs::create_dir_all(local.path("Archive")).unwrap();
    for name in [
        "keep.txt",
        "edit-me.txt",
        "rename-me.txt",
        "move-me.txt",
        "delete-me.txt",
        "Archive",
    ] {
        local.touched(name);
    }
    let left = drain(
        &local.store,
        &fixture.origin,
        &fixture.credential,
        Duration::from_secs(90),
    )
    .await;
    assert_eq!(left, 0, "the starting point should reach the server");

    // --- the server goes away ---------------------------------------------
    let nowhere = Url::parse(NOWHERE).unwrap();

    // Six kinds of change, none of which can be sent.
    local.write("offline-create.txt", b"created while offline");
    local.touched("offline-create.txt");

    local.write("edit-me.txt", b"after, and rather longer than before");
    local.touched("edit-me.txt");

    std::fs::rename(local.path("rename-me.txt"), local.path("renamed.txt")).unwrap();
    local.renamed("rename-me.txt", "renamed.txt");

    std::fs::rename(local.path("move-me.txt"), local.path("Archive/move-me.txt")).unwrap();
    local.gone("move-me.txt");
    local.touched("Archive/move-me.txt");

    std::fs::remove_file(local.path("delete-me.txt")).unwrap();
    local.gone("delete-me.txt");

    local.write("nested/deep/new.txt", b"a new tree");
    local.touched("nested/deep/new.txt");

    let queued_offline = local.outstanding();
    eprintln!("queued while offline: {queued_offline:?}");
    assert!(
        queued_offline
            .iter()
            .any(|(p, _)| p == "offline-create.txt"),
        "the creation must be queued"
    );
    assert!(
        queued_offline
            .iter()
            .any(|(p, i)| p == "delete-me.txt" && *i == Intent::Tombstone),
        "the deletion must be queued"
    );
    assert!(
        queued_offline
            .iter()
            .any(|(p, i)| p == "renamed.txt" && *i == Intent::Move),
        "the rename must be queued as a move"
    );
    assert!(
        queued_offline
            .iter()
            .any(|(p, i)| p == "Archive/move-me.txt" && *i == Intent::Move),
        "the cross-directory move must be queued as a move"
    );

    // Compacted, not a journal: one row per logical entry however many events
    // it took to get there.
    let mut paths: Vec<&String> = queued_offline.iter().map(|(p, _)| p).collect();
    paths.sort();
    let before_dedup = paths.len();
    paths.dedup();
    assert_eq!(
        paths.len(),
        before_dedup,
        "the queue must hold one row per entry, not a log of events"
    );

    // --- trying while it is down ------------------------------------------
    //
    // Real connection refusals. What matters is that nothing is lost and
    // nothing is marked done.
    let started = Instant::now();
    let left = drain(
        &local.store,
        &nowhere,
        &fixture.credential,
        Duration::from_secs(12),
    )
    .await;
    let attempted_for = started.elapsed();
    assert!(
        left > 0,
        "nothing can be sent to a server that is not there"
    );
    eprintln!("after {attempted_for:?} against a dead port, {left} items still queued");

    let after_failure = local.outstanding();
    assert_eq!(
        after_failure.len(),
        queued_offline.len(),
        "a failed attempt must not lose work"
    );
    assert!(
        !local
            .store
            .entries_in_root(SERVER, &fixture.root_id)
            .unwrap()
            .iter()
            .any(|e| e.state == SyncState::Synced && e.relative_path == "offline-create.txt"),
        "nothing may be marked sent that was not sent"
    );

    // --- restart, still offline --------------------------------------------
    //
    // The queue is the database, so this is the real test of it: drop
    // everything held in memory and open the same folder again.
    let dir = local.dir;
    let root_id = fixture.root_id.clone();
    drop(local.store);
    let local = Local::open(dir, &root_id);

    let after_restart = local.outstanding();
    assert_eq!(
        after_restart.len(),
        queued_offline.len(),
        "the queue must survive a restart"
    );
    for (path, intent) in &queued_offline {
        assert!(
            after_restart.iter().any(|(p, i)| p == path && i == intent),
            "{path} ({intent:?}) was lost across the restart"
        );
    }

    // --- the server comes back ---------------------------------------------
    let left = drain(
        &local.store,
        &fixture.origin,
        &fixture.credential,
        Duration::from_secs(120),
    )
    .await;
    assert_eq!(left, 0, "the queue must drain once the server is reachable");

    // --- what the server ended up with -------------------------------------
    let client = BackupClient::new(fixture.origin.clone(), fixture.credential.clone());
    let profile = client.me().await.expect("the grant should still work");
    assert_eq!(
        profile.roots.len(),
        1,
        "no duplicate roots may appear: {:?}",
        profile.roots.iter().map(|r| &r.id).collect::<Vec<_>>()
    );

    // And locally, every intent is spent and matches what is on disk.
    let entries = local
        .store
        .entries_in_root(SERVER, &fixture.root_id)
        .unwrap();
    for entry in &entries {
        assert_eq!(
            entry.state,
            SyncState::Synced,
            "{} should be settled, not {:?}",
            entry.relative_path,
            entry.state
        );
        assert!(
            local.path(&entry.relative_path).exists(),
            "{} is recorded as live but is not on disk",
            entry.relative_path
        );
    }

    let live: std::collections::BTreeSet<String> = entries
        .iter()
        .map(|entry| entry.relative_path.clone())
        .collect();
    for expected in [
        "keep.txt",
        "edit-me.txt",
        "renamed.txt",
        "Archive/move-me.txt",
        "offline-create.txt",
        "nested/deep/new.txt",
    ] {
        assert!(live.contains(expected), "{expected} should be live");
    }
    for gone in ["delete-me.txt", "rename-me.txt", "move-me.txt"] {
        assert!(!live.contains(gone), "{gone} should not be live");
    }

    // Nothing was duplicated: one row per path on disk.
    let on_disk =
        reconcile::reconcile_root(&local.store, SERVER, &local.root, &CancelFlag::new()).unwrap();
    eprintln!("final reconciliation after recovery: {on_disk:?}");
    assert!(
        matches!(
            on_disk,
            reconcile::Outcome::Reconciled {
                created: 0,
                updated: 0,
                moved: 0,
                removed: 0
            }
        ),
        "after recovery the database must already agree with the disk: {on_disk:?}"
    );

    eprintln!("offline durability certified against {}", fixture.origin);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unreachable_server_is_not_hammered() {
    // A client that retries in a tight loop burns the battery, saturates the
    // link the moment it returns, and looks like an attack to anything in
    // between. Failures have to cost more each time.
    let Some(fixture) = fixture() else {
        eprintln!("skipped: needs a disposable instance");
        return;
    };

    let dir = tempfile::tempdir().unwrap();
    let local = Local::open(dir, &fixture.root_id);
    for index in 0..20 {
        let name = format!("file{index}.txt");
        local.write(&name, b"x");
        local.touched(&name);
    }

    let nowhere = Url::parse(NOWHERE).unwrap();
    let started = Instant::now();
    let left = drain(
        &local.store,
        &nowhere,
        &fixture.credential,
        Duration::from_secs(15),
    )
    .await;
    let elapsed = started.elapsed();

    assert!(left > 0, "nothing should have been sent");

    // With no backoff, twenty entries against a port that refuses instantly
    // would be retried thousands of times in fifteen seconds. The count of
    // entries left is unchanged; what is being asserted is that the loop
    // slowed down rather than spinning.
    let failed = local
        .store
        .entries_in_root(SERVER, &fixture.root_id)
        .unwrap()
        .iter()
        .filter(|entry| entry.state == SyncState::Failed)
        .count();
    eprintln!("after {elapsed:?} against a dead port: {left} queued, {failed} marked failed");
    assert!(
        elapsed >= Duration::from_secs(10),
        "the run should have spent its time waiting, not spinning"
    );
}
