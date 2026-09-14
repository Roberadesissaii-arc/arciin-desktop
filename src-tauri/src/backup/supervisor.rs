//! Keeping the watcher, the queue and the folders in step with each other.
//!
//! The watcher reports changes, the reconciler establishes truth, and the
//! engine sends. Something has to decide *when* each of those happens, and
//! that decision is this module.
//!
//! # What it is responsible for
//!
//! * Registering exactly the folders that should be watched, and no others.
//! * Turning watcher changes into queued intent.
//! * Reconciling: on start, periodically, and whenever the watcher admits it
//!   lost events or a folder comes back.
//! * Stopping cleanly, so nothing keeps running after backup is switched off.
//!
//! # Why it reconciles even while the watcher is healthy
//!
//! Because a healthy watcher is not a complete one. It sees nothing while the
//! app is closed, drops events when Windows' buffer overflows, and cannot
//! report what happened during a crash. The periodic pass is what turns "we
//! think we are up to date" into something that becomes true again on a known
//! interval, and it is the only reason the client can claim correctness rather
//! than hope.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::backup::reconcile::{self, Outcome};
use crate::backup::scan::CancelFlag;
use crate::backup::store::{Root, RootStatus, SyncStore};
use crate::backup::watcher::{LocalChange, WatcherManager};
use crate::error::AppError;

/// How often every protected folder is read from scratch.
///
/// Long enough to be invisible — a full pass costs a directory walk, and doing
/// that every minute on a large folder would be felt — and short enough that
/// anything the watcher missed is corrected within a coffee break rather than
/// a working day.
pub const RECONCILE_INTERVAL: Duration = Duration::from_secs(600);

/// How often the loop wakes to see whether anything is owed.
///
/// Small relative to the interval so a folder that becomes available, or a
/// rescan the watcher asked for, is picked up promptly without polling the
/// filesystem.
const TICK: Duration = Duration::from_secs(5);

/// Folders asked for by something other than the clock.
///
/// A watcher overflow, a folder returning, waking from sleep. Kept as a set of
/// ids so a hundred dropped events cost one walk.
#[derive(Default)]
pub struct Pending {
    roots: Mutex<Vec<String>>,
    all: AtomicBool,
}

impl Pending {
    pub fn request(&self, root_id: &str) {
        let mut roots = self.roots.lock().unwrap();
        if !roots.iter().any(|id| id == root_id) {
            roots.push(root_id.to_string());
        }
    }

    pub fn request_all(&self) {
        self.all.store(true, Ordering::Relaxed);
    }

    /// Take what is owed, leaving nothing behind.
    pub fn take(&self) -> (Vec<String>, bool) {
        let mut roots = self.roots.lock().unwrap();
        let taken = std::mem::take(&mut *roots);
        (taken, self.all.swap(false, Ordering::Relaxed))
    }

    pub fn is_empty(&self) -> bool {
        self.roots.lock().unwrap().is_empty() && !self.all.load(Ordering::Relaxed)
    }
}

/// Which folders the watcher should be registered for.
///
/// Three conditions, all required, and each one is a mistake somebody could
/// make: the server still protects it (`enabled`), this PC can read it
/// (`status`), and it is actually a directory right now. Watching a folder
/// that fails any of them produces events nothing can act on — or worse,
/// events that look like mass deletion.
pub fn watchable(roots: &[Root]) -> Vec<Root> {
    roots
        .iter()
        .filter(|root| root.enabled)
        .filter(|root| root.status == RootStatus::Active)
        .filter(|root| reconcile::root_is_readable(&root.local_path))
        .cloned()
        .collect()
}

/// Everything the supervisor needs to do its job for one server.
pub struct Supervisor {
    pub watcher: Arc<WatcherManager>,
    pub pending: Arc<Pending>,
    stop: Arc<AtomicBool>,
}

impl Default for Supervisor {
    fn default() -> Self {
        Self::new()
    }
}

impl Supervisor {
    pub fn new() -> Self {
        Self {
            watcher: Arc::new(WatcherManager::new()),
            pending: Arc::new(Pending::default()),
            stop: Arc::new(AtomicBool::new(true)),
        }
    }

    pub fn is_running(&self) -> bool {
        !self.stop.load(Ordering::Relaxed)
    }

    /// Stop watching and stop reconciling.
    ///
    /// Called when backup is switched off, the profile is disabled, or the
    /// device is revoked. Nothing may keep running in the background after any
    /// of those: a watcher that survives a revocation is a client still
    /// reading a person's folders after they told it not to.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        self.watcher.stop();
    }

    /// Begin watching and reconciling for one server.
    ///
    /// Safe to call again: registrations are computed as a difference, and the
    /// loop is single — a second call adjusts what is watched rather than
    /// starting a second everything.
    pub fn start(&self, store: Arc<SyncStore>, server_id: String) -> Result<(), AppError> {
        self.stop.store(false, Ordering::Relaxed);
        self.refresh(&store, &server_id)?;

        let watcher = Arc::clone(&self.watcher);
        let pending = Arc::clone(&self.pending);
        let stop = Arc::clone(&self.stop);
        let store_for_sink = Arc::clone(&store);

        tauri::async_runtime::spawn(async move {
            let mut last_full = std::time::Instant::now();
            // The first pass happens immediately: the app has just started, and
            // whatever changed while it was closed is waiting to be found.
            let mut due_now = true;

            let mut last_tick = std::time::Instant::now();

            while !stop.load(Ordering::Relaxed) {
                // A tick that took far longer than it was asked to almost
                // always means the machine slept. Windows does not replay the
                // notifications missed while suspended, and the handles may not
                // have survived, so the folders are read rather than assumed.
                //
                // Inferred from the clock rather than a power-event
                // subscription because the conclusion is the same for every
                // cause — suspend, hibernate, a stalled disk, the process
                // starved of CPU — and all of them mean the event stream has a
                // hole in it.
                let slept = last_tick.elapsed() > TICK * 4;
                if slept {
                    tracing::info!(
                        gap_s = last_tick.elapsed().as_secs(),
                        "a long gap suggests this computer was asleep; re-reading every folder"
                    );
                }
                last_tick = std::time::Instant::now();

                let (requested, all) = pending.take();
                let full = all || slept || due_now || last_full.elapsed() >= RECONCILE_INTERVAL;
                due_now = false;

                if full || !requested.is_empty() {
                    let store = Arc::clone(&store);
                    let server = server_id.clone();
                    let only = if full { None } else { Some(requested) };

                    // Off the async runtime: a directory walk is blocking work
                    // and would otherwise stall every other task on the thread.
                    let outcome = tauri::async_runtime::spawn_blocking(move || {
                        reconcile_pass(&store, &server, only.as_deref())
                    })
                    .await;

                    match outcome {
                        Ok(Ok(roots)) => {
                            if full {
                                last_full = std::time::Instant::now();
                            }
                            // Folders may have become available, unavailable,
                            // or held. The registration set follows.
                            let _ = watcher.watch(
                                &watchable(&roots),
                                sink(
                                    Arc::clone(&store_for_sink),
                                    server_id.clone(),
                                    Arc::clone(&pending),
                                ),
                            );
                        }
                        Ok(Err(err)) => {
                            tracing::warn!(code = %err.code, "a reconciliation pass failed")
                        }
                        Err(_) => tracing::warn!("a reconciliation pass was interrupted"),
                    }
                }

                tokio::time::sleep(TICK).await;
            }

            tracing::info!("the backup supervisor stopped");
        });

        Ok(())
    }

    /// Register the watcher for whatever is currently watchable.
    pub fn refresh(&self, store: &Arc<SyncStore>, server_id: &str) -> Result<(), AppError> {
        let roots = store.roots(server_id)?;
        self.watcher.watch(
            &watchable(&roots),
            sink(
                Arc::clone(store),
                server_id.to_string(),
                Arc::clone(&self.pending),
            ),
        )?;
        Ok(())
    }
}

/// Where the watcher's changes go.
///
/// Applied here, on the watcher's own thread, rather than queued in memory.
/// The queue is the database; holding a second one in RAM would mean a crash
/// loses work that looked accepted. As it is, a crash loses at most the few
/// hundred milliseconds the debouncer was still holding — and reconciliation
/// finds even that.
fn sink(
    store: Arc<SyncStore>,
    server_id: String,
    pending: Arc<Pending>,
) -> Arc<dyn Fn(Vec<LocalChange>) + Send + Sync> {
    Arc::new(move |changes: Vec<LocalChange>| {
        // Read once per batch: a burst of a thousand events should not be a
        // thousand queries for the same folder list.
        let roots = match store.roots(&server_id) {
            Ok(roots) => roots,
            Err(err) => {
                tracing::warn!(code = %err.code, "local changes could not be placed");
                return;
            }
        };

        for change in changes {
            if matches!(change, LocalChange::RescanNeeded { .. }) {
                // Not acted on here. Losing events means this folder's state is
                // unknown, and the answer to unknown is to go and read it.
                pending.request(change.root_id());
                continue;
            }
            let Some(root) = roots.iter().find(|root| root.id == change.root_id()) else {
                continue;
            };
            if let Err(err) = reconcile::apply_change(&store, &server_id, root, &change) {
                tracing::warn!(
                    code = %err.code,
                    root_id = %root.id,
                    "a local change could not be queued"
                );
            }
        }
    })
}

/// Reconcile every protected folder, or only the ones named.
///
/// Returns the folders as they stand afterwards, so the caller can adjust what
/// is watched without reading them again.
pub fn reconcile_pass(
    store: &SyncStore,
    server_id: &str,
    only: Option<&[String]>,
) -> Result<Vec<Root>, AppError> {
    let roots = store.roots(server_id)?;
    let cancel = CancelFlag::new();

    for root in &roots {
        if !root.enabled {
            continue;
        }
        if let Some(only) = only {
            if !only.iter().any(|id| id == &root.id) {
                continue;
            }
        }
        // A folder held after a mass change stays held. Walking it again would
        // reach the same conclusion and re-queue the same removals, which is
        // the opposite of holding.
        if root.status == RootStatus::SafetyHold {
            continue;
        }

        let started = std::time::Instant::now();
        match reconcile::reconcile_root(store, server_id, root, &cancel) {
            Ok(Outcome::Reconciled {
                created,
                updated,
                moved,
                removed,
            }) => {
                if created + updated + moved + removed > 0 {
                    tracing::info!(
                        root_id = %root.id,
                        created,
                        updated,
                        moved,
                        removed,
                        ms = started.elapsed().as_millis() as u64,
                        "reconciled a protected folder"
                    );
                }
            }
            Ok(Outcome::Unavailable) => tracing::info!(
                root_id = %root.id,
                "a protected folder is unavailable; nothing was treated as deleted"
            ),
            Ok(Outcome::MassChangeHeld { missing, known }) => tracing::warn!(
                root_id = %root.id,
                missing,
                known,
                "a protected folder is held after an unusually large change"
            ),
            Err(err) => tracing::warn!(
                root_id = %root.id,
                code = %err.code,
                "a protected folder could not be reconciled"
            ),
        }
    }

    store.roots(server_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn root(id: &str, path: PathBuf, enabled: bool, status: RootStatus) -> Root {
        Root {
            id: id.into(),
            kind: "CUSTOM".into(),
            display_name: id.into(),
            local_path: path,
            enabled,
            status,
        }
    }

    #[test]
    fn a_protected_readable_folder_is_watched() {
        let dir = tempfile::tempdir().unwrap();
        let roots = [root(
            "r1",
            dir.path().to_path_buf(),
            true,
            RootStatus::Active,
        )];
        assert_eq!(watchable(&roots).len(), 1);
    }

    #[test]
    fn a_folder_the_server_has_switched_off_is_not_watched() {
        // Watching it would queue work for a root the server refuses writes
        // for, turning one clear answer into an error per file.
        let dir = tempfile::tempdir().unwrap();
        let roots = [root(
            "r1",
            dir.path().to_path_buf(),
            false,
            RootStatus::Active,
        )];
        assert!(watchable(&roots).is_empty());
    }

    #[test]
    fn a_folder_that_cannot_be_read_is_not_watched() {
        let roots = [root(
            "r1",
            PathBuf::from(r"Q:\NoSuchVolume\Protected"),
            true,
            RootStatus::Active,
        )];
        assert!(watchable(&roots).is_empty());
    }

    #[test]
    fn a_folder_held_after_a_mass_change_is_not_watched() {
        // The hold exists to stop acting on that folder. Continuing to watch
        // it would keep queueing exactly the work the hold is refusing to do.
        let dir = tempfile::tempdir().unwrap();
        let roots = [root(
            "r1",
            dir.path().to_path_buf(),
            true,
            RootStatus::SafetyHold,
        )];
        assert!(watchable(&roots).is_empty());
    }

    #[test]
    fn an_unavailable_folder_is_not_watched() {
        let dir = tempfile::tempdir().unwrap();
        let roots = [root(
            "r1",
            dir.path().to_path_buf(),
            true,
            RootStatus::Unavailable,
        )];
        assert!(watchable(&roots).is_empty());
    }

    #[test]
    fn only_the_folders_that_qualify_are_watched() {
        let dir = tempfile::tempdir().unwrap();
        let roots = [
            root("keep", dir.path().to_path_buf(), true, RootStatus::Active),
            root("off", dir.path().to_path_buf(), false, RootStatus::Active),
            root(
                "gone",
                PathBuf::from(r"Q:\NoSuchVolume"),
                true,
                RootStatus::Active,
            ),
        ];
        let watched = watchable(&roots);
        assert_eq!(watched.len(), 1);
        assert_eq!(watched[0].id, "keep");
    }

    // --- Coalescing requests ----------------------------------------------

    #[test]
    fn asking_for_the_same_folder_twice_costs_one_walk() {
        // A watcher overflow can report the same folder many times over. Each
        // one must not become its own directory walk.
        let pending = Pending::default();
        pending.request("r1");
        pending.request("r1");
        pending.request("r2");
        let (roots, all) = pending.take();
        assert_eq!(roots.len(), 2);
        assert!(!all);
    }

    #[test]
    fn taking_the_requests_leaves_nothing_behind() {
        let pending = Pending::default();
        pending.request("r1");
        let _ = pending.take();
        assert!(pending.is_empty());
        let (roots, all) = pending.take();
        assert!(roots.is_empty());
        assert!(!all);
    }

    #[test]
    fn a_request_for_everything_is_remembered_separately() {
        let pending = Pending::default();
        pending.request_all();
        assert!(!pending.is_empty());
        let (_, all) = pending.take();
        assert!(all);
        assert!(pending.is_empty());
    }
}
