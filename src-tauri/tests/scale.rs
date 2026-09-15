//! Engineering sanity numbers for a folder with ten thousand files in it.
//!
//! Not a benchmark. The question is not "how fast" but "is anything here
//! pathological" — minutes to walk a small tree, hundreds of megabytes of
//! database for metadata, a queue that grows without bound, work that never
//! settles. Those are the failures worth catching, and they are visible at an
//! order of magnitude, not a percentage.
//!
//! Ignored by default: it writes ten thousand files, which is rude to do on
//! every `cargo test`. Run it deliberately:
//!
//! ```text
//! cargo test --test scale -- --ignored --nocapture
//! ```

use std::sync::Arc;
use std::time::Instant;

use arciin_desktop_lib::backup::reconcile::{self, Outcome};
use arciin_desktop_lib::backup::scan::CancelFlag;
use arciin_desktop_lib::backup::store::{Profile, Root, RootStatus, SyncState, SyncStore};

const SERVER: &str = "3f2504e0-4f89-41d3-9a0c-0305e82c3301";
const ROOT: &str = "root-1";

/// Nested rather than flat, because that is what a real folder looks like and
/// a flat directory of ten thousand entries exercises a different, easier path.
const DIRS: usize = 100;
const PER_DIR: usize = 100;

fn database_bytes(dir: &std::path::Path) -> u64 {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|entry| entry.metadata().ok())
                .map(|meta| meta.len())
                .sum()
        })
        .unwrap_or(0)
}

#[test]
#[ignore = "writes 10,000 files; run deliberately"]
fn ten_thousand_files() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let protected = dir.path().join("Protected");

    // --- fixture ---------------------------------------------------------
    let building = Instant::now();
    let mut total_bytes = 0u64;
    for d in 0..DIRS {
        let sub = protected
            .join(format!("group{:03}", d / 10))
            .join(format!("dir{d:03}"));
        std::fs::create_dir_all(&sub).unwrap();
        for f in 0..PER_DIR {
            let body = format!("file {d}/{f} with a little content so it is not empty\n");
            total_bytes += body.len() as u64;
            std::fs::write(sub.join(format!("file{f:03}.txt")), body).unwrap();
        }
    }
    eprintln!(
        "fixture: {} files, {:.1} MB, built in {:?}",
        DIRS * PER_DIR,
        total_bytes as f64 / 1024.0 / 1024.0,
        building.elapsed()
    );

    let store = Arc::new(SyncStore::open(&state).unwrap());
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
        local_path: protected.clone(),
        enabled: true,
        status: RootStatus::Active,
    };
    store.save_root(SERVER, &root).unwrap();
    let db_empty = database_bytes(&state);

    // --- initial reconciliation -------------------------------------------
    let started = Instant::now();
    let outcome = reconcile::reconcile_root(&store, SERVER, &root, &CancelFlag::new()).unwrap();
    let first_pass = started.elapsed();
    let Outcome::Reconciled { created, .. } = outcome else {
        panic!("expected a reconciliation, got {outcome:?}");
    };
    let db_after_inventory = database_bytes(&state);
    eprintln!(
        "initial reconcile: {created} entries queued in {first_pass:?} \
         ({:.0} files/s)",
        created as f64 / first_pass.as_secs_f64().max(0.001)
    );
    eprintln!(
        "sqlite: {} KB empty -> {} KB after inventory",
        db_empty / 1024,
        db_after_inventory / 1024
    );
    assert!(created >= DIRS * PER_DIR, "every file should be queued");

    // --- a second pass with nothing to do ---------------------------------
    //
    // The number that decides whether the periodic pass is invisible or
    // felt. It runs every ten minutes forever, and almost always has nothing
    // to do; if that costs as much as the first pass, it is a problem.
    let started = Instant::now();
    let outcome = reconcile::reconcile_root(&store, SERVER, &root, &CancelFlag::new()).unwrap();
    let idle_pass = started.elapsed();
    let Outcome::Reconciled {
        created, updated, ..
    } = outcome
    else {
        panic!("expected a reconciliation, got {outcome:?}");
    };
    eprintln!("idle reconcile: {idle_pass:?}, {created} created, {updated} updated");
    assert_eq!(created, 0, "a second look must find nothing new");
    assert_eq!(updated, 0, "and nothing changed");

    // --- drain -------------------------------------------------------------
    let started = Instant::now();
    let mut drained = 0usize;
    loop {
        let batch = store.outstanding(SERVER, 500).unwrap();
        if batch.is_empty() {
            break;
        }
        for entry in &batch {
            store
                .complete_operation(SERVER, &entry.client_entry_id, SyncState::Synced)
                .unwrap();
            drained += 1;
        }
    }
    let drain = started.elapsed();
    let db_after_drain = database_bytes(&state);
    eprintln!(
        "drained {drained} entries in {drain:?}; sqlite {} KB after drain",
        db_after_drain / 1024
    );

    // --- burst --------------------------------------------------------------
    //
    // 100 modified, 100 created, 100 deleted, all at once. What matters is
    // that the work produced is proportional to what changed, not to the size
    // of the folder.
    let burst_dir = protected.join("group000").join("dir000");
    for f in 0..100 {
        std::fs::write(
            burst_dir.join(format!("file{f:03}.txt")),
            format!("modified {f} with different length to be visible\n"),
        )
        .unwrap();
    }
    let fresh = protected.join("burst");
    std::fs::create_dir_all(&fresh).unwrap();
    for f in 0..100 {
        std::fs::write(fresh.join(format!("new{f:03}.txt")), b"new").unwrap();
    }
    let doomed = protected.join("group000").join("dir001");
    for f in 0..100 {
        std::fs::remove_file(doomed.join(format!("file{f:03}.txt"))).unwrap();
    }

    let started = Instant::now();
    let outcome = reconcile::reconcile_root(&store, SERVER, &root, &CancelFlag::new()).unwrap();
    let burst_pass = started.elapsed();
    let Outcome::Reconciled {
        created,
        updated,
        removed,
        ..
    } = outcome
    else {
        panic!("expected a reconciliation, got {outcome:?}");
    };
    let peak_queue = store.outstanding(SERVER, 100_000).unwrap().len();
    eprintln!(
        "burst: {created} created, {updated} updated, {removed} removed in {burst_pass:?}; \
         queue peak {peak_queue}"
    );

    assert!(
        peak_queue < 400,
        "the queue should hold what changed, not the whole folder: {peak_queue}"
    );
    assert_eq!(removed, 100, "the deletions should be found");

    // --- the numbers that would be pathological ----------------------------
    assert!(
        first_pass.as_secs() < 60,
        "a 10k walk taking a minute would be pathological: {first_pass:?}"
    );
    assert!(
        db_after_drain < 64 * 1024 * 1024,
        "metadata for 10k files should not cost tens of megabytes: {} KB",
        db_after_drain / 1024
    );
    assert!(
        idle_pass < first_pass * 2,
        "an idle pass must not cost more than the first one"
    );
}
