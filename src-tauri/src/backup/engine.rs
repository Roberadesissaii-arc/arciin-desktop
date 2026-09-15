//! The backup engine: what actually moves files to Arciin.
//!
//! It is a queue, not a traversal. A scan writes entries into the local
//! database marked `PENDING`; the engine drains whatever is outstanding. That
//! split is what makes the whole thing restartable — the queue is on disk, so
//! closing the app mid-backup loses nothing and duplicates nothing.
//!
//! ```txt
//! scan  ──▶  SQLite (PENDING)  ──▶  engine  ──▶  Arciin
//!                  ▲                    │
//!                  └──── watcher ───────┘
//! ```
//!
//! Three rules shape everything here:
//!
//! 1. **Never claim success the server did not confirm.** An entry becomes
//!    `SYNCED` only after a 2xx, never before.
//! 2. **Record the operation id before sending it.** A crash mid-flight then
//!    leaves a retryable record whose replay the server recognises, instead of
//!    an unknown state that risks a duplicate.
//! 3. **Classify failures.** Backoff is for transient trouble; a revoked grant
//!    stops the engine rather than hammering the server.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Semaphore;

use crate::backup::client::{classify, BackupClient, Retry};
use crate::backup::protocol::BackupHealth;
use crate::backup::store::{Entry, EntryType, Intent, RootStatus, SyncState, SyncStore};
use crate::error::AppError;

/// Simultaneous transfers.
///
/// Deliberately small. The bottleneck is usually the disk or a home LAN, and
/// beyond a handful of streams throughput stops improving while memory,
/// contention and the server's rate limit all get worse.
const MAX_CONCURRENT_TRANSFERS: usize = 3;

/// Entries pulled from the queue per pass.
const BATCH_SIZE: usize = 64;

/// Retry schedule. Capped so a long outage settles into a slow poll rather
/// than either giving up or spinning.
const BACKOFF_BASE: Duration = Duration::from_secs(2);
const BACKOFF_MAX: Duration = Duration::from_secs(300);

/// The server persists heartbeats about once a minute; sending faster only
/// burns requests.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(60);

/// How long to idle when the queue is empty before looking again.
const IDLE_POLL: Duration = Duration::from_secs(15);

/// Live counters for the status surface.
#[derive(Default)]
pub struct EngineStats {
    pub files_done: AtomicU64,
    pub files_failed: AtomicU64,
    pub bytes_done: AtomicU64,
    /// Consecutive transient failures, which drives the backoff.
    pub consecutive_failures: AtomicU64,
}

/// A snapshot of engine state, safe to hand to the UI.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineStatus {
    pub health: String,
    pub files_synced: i64,
    pub files_outstanding: i64,
    pub files_failed: i64,
    pub bytes_synced: i64,
    /// Still to send. Lets the UI show a size, not just a file count.
    pub bytes_outstanding: i64,
    pub paused: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

/// Shared control surface. Cloneable so commands can pause a running engine.
#[derive(Clone, Default)]
pub struct EngineControl {
    paused: Arc<AtomicBool>,
    stopped: Arc<AtomicBool>,
}

impl EngineControl {
    pub fn new() -> Self {
        Self::default()
    }

    /// Stop starting new transfers. The queue and all state are kept, so
    /// resuming picks up exactly where it left off.
    pub fn pause(&self) {
        self.paused.store(true, Ordering::Relaxed);
    }

    pub fn resume(&self) {
        self.paused.store(false, Ordering::Relaxed);
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    /// Shut the engine down entirely (revocation, disable, app exit).
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::Relaxed);
    }

    pub fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::Relaxed)
    }
}

/// Exponential backoff with jitter.
///
/// The jitter matters more than the exponent: without it, every queued entry
/// that failed during one outage retries at the same instant when the server
/// returns, which is how a recovering server gets knocked over again.
pub fn backoff_delay(consecutive_failures: u64) -> Duration {
    use rand::Rng;

    if consecutive_failures == 0 {
        return Duration::ZERO;
    }
    let exponent = consecutive_failures.min(8) as u32;
    let base = BACKOFF_BASE.saturating_mul(1u32 << (exponent - 1));
    let capped = base.min(BACKOFF_MAX);

    // Full jitter: anywhere in [0, capped].
    let millis = capped.as_millis() as u64;
    Duration::from_millis(rand::rng().random_range(0..=millis.max(1)))
}

/// A fresh idempotency key.
///
/// Generated once per logical operation and persisted *before* the request, so
/// every retry of that operation carries the same key and the server replays
/// its original result instead of creating a second object.
fn new_operation_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

pub struct BackupEngine {
    server_id: String,
    client: Arc<BackupClient>,
    store: Arc<SyncStore>,
    control: EngineControl,
    stats: Arc<EngineStats>,
    last_error: Arc<std::sync::Mutex<Option<String>>>,
}

impl BackupEngine {
    pub fn new(
        server_id: String,
        client: Arc<BackupClient>,
        store: Arc<SyncStore>,
        control: EngineControl,
    ) -> Self {
        Self {
            server_id,
            client,
            store,
            control,
            stats: Arc::new(EngineStats::default()),
            last_error: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    pub fn stats(&self) -> Arc<EngineStats> {
        Arc::clone(&self.stats)
    }

    /// Current state for the UI.
    pub fn status(&self) -> Result<EngineStatus, AppError> {
        let (synced, outstanding, failed, bytes, pending_bytes) =
            self.store.progress(&self.server_id)?;
        let last_error = self.last_error.lock().unwrap().clone();
        let health = if self.control.is_paused() {
            BackupHealth::Paused
        } else if last_error.is_some() {
            BackupHealth::Error
        } else if outstanding > 0 {
            BackupHealth::Syncing
        } else {
            BackupHealth::UpToDate
        };

        Ok(EngineStatus {
            health: health.as_str().to_string(),
            files_synced: synced,
            files_outstanding: outstanding,
            files_failed: failed,
            bytes_synced: bytes,
            bytes_outstanding: pending_bytes,
            paused: self.control.is_paused(),
            last_error,
        })
    }

    /// Drain the queue until stopped.
    ///
    /// Returns `Ok(())` on a clean stop, or the fatal error that ended it — a
    /// revoked grant, say — so the caller can clear credentials and tell the
    /// user rather than retry forever.
    pub async fn run(&self) -> Result<(), AppError> {
        tracing::info!(server_id = %self.server_id, "backup engine started");
        let mut last_heartbeat = std::time::Instant::now() - HEARTBEAT_INTERVAL;

        while !self.control.is_stopped() {
            if self.control.is_paused() {
                self.maybe_heartbeat(&mut last_heartbeat, BackupHealth::Paused)
                    .await;
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }

            let batch = self.store.outstanding(&self.server_id, BATCH_SIZE)?;
            if batch.is_empty() {
                self.maybe_heartbeat(&mut last_heartbeat, BackupHealth::UpToDate)
                    .await;
                tokio::time::sleep(IDLE_POLL).await;
                continue;
            }

            self.maybe_heartbeat(&mut last_heartbeat, BackupHealth::Syncing)
                .await;

            match self.process_batch(batch).await {
                Ok(()) => {}
                Err(fatal) => {
                    tracing::warn!(code = %fatal.code, "backup engine stopping");
                    *self.last_error.lock().unwrap() = Some(fatal.message.clone());
                    let owned = self.owned_roots();
                    let _ = self
                        .client
                        .heartbeat(BackupHealth::Error, Some(&fatal.code), owned.as_deref())
                        .await;
                    return Err(fatal);
                }
            }

            // A transient failure streak slows the whole loop, not just the
            // entry that failed — the network is usually the shared cause.
            let failures = self.stats.consecutive_failures.load(Ordering::Relaxed);
            if failures > 0 {
                let delay = backoff_delay(failures);
                tracing::info!(
                    failures,
                    delay_ms = delay.as_millis() as u64,
                    "backing off before the next batch"
                );
                tokio::time::sleep(delay).await;
            }
        }

        tracing::info!(server_id = %self.server_id, "backup engine stopped");
        Ok(())
    }

    /// Send one batch, bounded to `MAX_CONCURRENT_TRANSFERS` at a time.
    ///
    /// Folders come out of the queue first (the store orders them ahead of
    /// files), so a directory exists before anything lands inside it.
    async fn process_batch(&self, batch: Vec<Entry>) -> Result<(), AppError> {
        // Folders first, one at a time.
        //
        // The queue already hands them over shallowest-first, but ordering
        // alone is not enough: this used to spawn the whole batch at once, so
        // `WebProject` and `WebProject/public` ran concurrently and the child
        // could reach the server first. The parent's own create then came back
        // `ALREADY_EXISTS`. Creating a folder is a single small request, so
        // doing them in sequence costs almost nothing and makes
        // parent-before-child a property of the code rather than a race that
        // idempotency happens to cover.
        let (folders, files): (Vec<Entry>, Vec<Entry>) = batch
            .into_iter()
            .partition(|entry| entry.entry_type == EntryType::Folder);

        let roots = self.store.roots(&self.server_id)?;
        for entry in folders {
            if self.control.is_stopped() || self.control.is_paused() {
                return Ok(());
            }
            send_entry(
                &self.server_id,
                &self.client,
                &self.store,
                &self.stats,
                &roots,
                entry,
            )
            .await?;
        }

        // Files have no dependency on each other, so they keep their bounded
        // concurrency — and by now every folder they need exists.
        let semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT_TRANSFERS));
        let mut handles = Vec::new();

        for entry in files {
            if self.control.is_stopped() || self.control.is_paused() {
                break;
            }
            let permit = Arc::clone(&semaphore)
                .acquire_owned()
                .await
                .map_err(|_| AppError::internal("Backup scheduler stopped unexpectedly."))?;

            let server_id = self.server_id.clone();
            let client = Arc::clone(&self.client);
            let store = Arc::clone(&self.store);
            let stats = Arc::clone(&self.stats);
            let roots = self.store.roots(&self.server_id)?;

            handles.push(tokio::spawn(async move {
                let _permit = permit;
                send_entry(&server_id, &client, &store, &stats, &roots, entry).await
            }));
        }

        // A fatal outcome from any transfer ends the whole run; the rest are
        // already recorded in the database and will be picked up again.
        for handle in handles {
            match handle.await {
                Ok(Err(fatal)) => return Err(fatal),
                Ok(Ok(())) => {}
                Err(err) => {
                    tracing::error!(error = %err, "a transfer task panicked");
                }
            }
        }
        Ok(())
    }

    async fn maybe_heartbeat(&self, last: &mut std::time::Instant, health: BackupHealth) {
        if last.elapsed() < HEARTBEAT_INTERVAL {
            return;
        }
        *last = std::time::Instant::now();
        let owned = self.owned_roots();
        if let Err(err) = self.client.heartbeat(health, None, owned.as_deref()).await {
            // A missed heartbeat is not worth interrupting a backup for.
            tracing::info!(code = %err.code, "heartbeat failed");
        }
    }

    /// The folders this computer claims, read fresh from local state.
    ///
    /// Read per heartbeat rather than cached at startup, so a folder the user
    /// removes stops being claimed on the next beat instead of at the next
    /// restart. Derived from the stored configuration and never from which
    /// watchers happen to be registered: a folder on an unplugged drive holds
    /// no watcher and is still very much this computer's to look after.
    ///
    /// On a read failure this returns `None`, which omits the field. Saying
    /// nothing is right here — an empty list would read as "this computer owns
    /// nothing", and the server would be entitled to act on it.
    fn owned_roots(&self) -> Option<Vec<String>> {
        match self.store.owned_root_identifiers(&self.server_id) {
            Ok(owned) => Some(owned),
            Err(err) => {
                tracing::warn!(code = %err.code, "owned roots unreadable; omitting from heartbeat");
                None
            }
        }
    }
}

/// Send one entry, updating local state to match what the server confirmed.
async fn send_entry(
    server_id: &str,
    client: &BackupClient,
    store: &SyncStore,
    stats: &EngineStats,
    roots: &[crate::backup::store::Root],
    entry: Entry,
) -> Result<(), AppError> {
    let Some(root) = roots.iter().find(|r| r.id == entry.root_id) else {
        // The root was removed under us; drop the entry rather than guess.
        store.complete_operation(server_id, &entry.client_entry_id, SyncState::Failed)?;
        return Ok(());
    };
    if !root.enabled {
        return Ok(());
    }
    // A folder that cannot be read, or one being held after a mass change, is
    // not a folder to act on. Its queue stays exactly as it is until somebody
    // or something resolves the state.
    if root.status != RootStatus::Active {
        return Ok(());
    }

    // Reuse the key from an interrupted attempt; otherwise mint one. Either
    // way it is persisted before the request leaves.
    let operation_id = entry
        .pending_operation_id
        .clone()
        .unwrap_or_else(new_operation_id);
    store.begin_operation(server_id, &entry.client_entry_id, &operation_id)?;

    // Removal first, because it is the one intent that does not care what is
    // on disk — by the time it is queued the thing is already gone.
    if entry.intent == Intent::Tombstone {
        let result = client
            .tombstone_entry(&root.id, &entry.client_entry_id, &operation_id)
            .await;
        return finish(
            server_id,
            store,
            stats,
            &entry,
            result.map(|_| ()),
            SyncState::Tombstoned,
        );
    }

    // A move keeps the entry's identity, so the server keeps its history and
    // its bytes. The alternative — upload to the new path, tombstone the old —
    // re-sends the whole file to say something the server could have been told
    // in one small request, and briefly shows the user two copies.
    if entry.intent == Intent::Move {
        let result = client
            .move_entry(
                &root.id,
                &entry.client_entry_id,
                &entry.relative_path,
                &operation_id,
            )
            .await;
        return finish(
            server_id,
            store,
            stats,
            &entry,
            result.map(|_| ()),
            SyncState::Synced,
        );
    }

    let outcome = match entry.entry_type {
        EntryType::Folder => {
            // A folder that is gone locally is a removal, exactly as a file
            // would be. Without this a deleted directory is re-created on the
            // server on every pass.
            let absolute = root.local_path.join(entry.relative_path.replace('/', "\\"));
            if !absolute.is_dir() {
                tracing::info!("a queued folder no longer exists; recording a tombstone");
                let result = client
                    .tombstone_entry(&root.id, &entry.client_entry_id, &operation_id)
                    .await;
                return finish(
                    server_id,
                    store,
                    stats,
                    &entry,
                    result.map(|_| ()),
                    SyncState::Tombstoned,
                );
            }
            client
                .create_folder(
                    &root.id,
                    &entry.client_entry_id,
                    &entry.relative_path,
                    &operation_id,
                )
                .await
                .map(|_| ())
        }
        EntryType::File => {
            let absolute = root.local_path.join(entry.relative_path.replace('/', "\\"));

            // A file that vanished between scan and send is a deletion, not a
            // failure. Tombstone it so Arciin matches the PC.
            if !absolute.exists() {
                tracing::info!("a queued file no longer exists; recording a tombstone");
                let result = client
                    .tombstone_entry(&root.id, &entry.client_entry_id, &operation_id)
                    .await;
                return finish(
                    server_id,
                    store,
                    stats,
                    &entry,
                    result.map(|_| ()),
                    SyncState::Tombstoned,
                );
            }

            // Still being written? Then not yet.
            //
            // A large file arrives over seconds or minutes — a download, a
            // video export, a copy from a slow drive — and the watcher reports
            // it long before it is finished. Uploading now would send a
            // truncated file and, worse, mark it synced: the server would hold
            // a corrupt copy and nothing would ever revisit it.
            //
            // Leaving it PENDING costs one more pass. The engine comes back
            // within seconds and the file is either finished or still growing,
            // in which case it waits again. There is no upper bound on how
            // long a legitimate write may take, so there is no timeout here —
            // only a refusal to send something that is visibly still changing.
            if let Some(unstable) = still_being_written(&absolute) {
                tracing::info!(
                    grew_by = unstable,
                    "a file is still being written; leaving it queued"
                );
                store.complete_operation(server_id, &entry.client_entry_id, SyncState::Pending)?;
                return Ok(());
            }

            // Re-read metadata: if the file changed while queued, the bytes
            // about to be sent are already the newer ones, and the recorded
            // size must match what was actually uploaded.
            if let Ok(metadata) = std::fs::metadata(&absolute) {
                let size = metadata.len() as i64;
                if size != entry.size_bytes {
                    let mut refreshed = entry.clone();
                    refreshed.size_bytes = size;
                    store.upsert_entry(server_id, &refreshed)?;
                }
            }

            client
                .upload_file(
                    &root.id,
                    &entry.client_entry_id,
                    &entry.relative_path,
                    &operation_id,
                    &absolute,
                )
                .await
                .map(|_| ())
        }
    };

    finish(server_id, store, stats, &entry, outcome, SyncState::Synced)
}

/// How much a file grew while we watched it, if it is still changing.
///
/// Two samples a short interval apart. Cheap, bounded, and it answers the only
/// question that matters before an upload: is anybody still writing to this?
///
/// Deliberately not a lock and not an exclusive open. Taking either would make
/// this client the reason somebody's save failed, which is a far worse failure
/// than uploading a file a few seconds later than it could have.
///
/// `None` means stable, or unreadable — an unreadable file is left to the
/// upload itself to fail honestly, rather than being silently deferred forever.
pub fn still_being_written(path: &std::path::Path) -> Option<u64> {
    const SETTLE: Duration = Duration::from_millis(400);

    let first = std::fs::metadata(path).ok()?;
    // Only files that look recently touched are worth pausing for. An old
    // file is not being written, and sampling every queued entry would add
    // this delay to every upload in a large backup.
    let recent = first
        .modified()
        .ok()
        .and_then(|at| at.elapsed().ok())
        .is_none_or(|since| since < Duration::from_secs(5));
    if !recent {
        return None;
    }

    std::thread::sleep(SETTLE);

    let second = std::fs::metadata(path).ok()?;
    if second.len() != first.len() {
        return Some(second.len().saturating_sub(first.len()));
    }
    // A file being rewritten in place keeps its length and changes its
    // timestamp, so length alone is not enough.
    if second.modified().ok() != first.modified().ok() {
        return Some(0);
    }
    None
}

/// Record the result of one operation.
fn finish(
    server_id: &str,
    store: &SyncStore,
    stats: &EngineStats,
    entry: &Entry,
    outcome: Result<(), AppError>,
    success_state: SyncState,
) -> Result<(), AppError> {
    match outcome {
        Ok(()) => {
            store.complete_operation(server_id, &entry.client_entry_id, success_state)?;
            stats.consecutive_failures.store(0, Ordering::Relaxed);
            if entry.entry_type == EntryType::File && success_state == SyncState::Synced {
                stats.files_done.fetch_add(1, Ordering::Relaxed);
                stats
                    .bytes_done
                    .fetch_add(entry.size_bytes.max(0) as u64, Ordering::Relaxed);
            }
            Ok(())
        }
        Err(err) => match classify(&err) {
            Retry::Fatal => Err(err),
            Retry::SkipEntry => {
                tracing::warn!(code = %err.code, "entry permanently rejected; skipping it");
                store.complete_operation(server_id, &entry.client_entry_id, SyncState::Failed)?;
                stats.files_failed.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            Retry::Backoff => {
                tracing::info!(code = %err.code, "entry will be retried");
                // Left FAILED *with* its operation id, so the retry replays the
                // same key rather than creating a second object.
                stats.consecutive_failures.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_then_settles() {
        // Full jitter means only the ceiling is deterministic.
        let mut previous_ceiling = Duration::ZERO;
        for failures in 1..=6 {
            let ceiling = BACKOFF_BASE
                .saturating_mul(1 << (failures - 1))
                .min(BACKOFF_MAX);
            assert!(ceiling >= previous_ceiling, "ceiling must not shrink");
            previous_ceiling = ceiling;

            for _ in 0..20 {
                assert!(backoff_delay(failures as u64) <= ceiling);
            }
        }
    }

    #[test]
    fn backoff_is_capped() {
        for _ in 0..50 {
            assert!(backoff_delay(64) <= BACKOFF_MAX);
        }
    }

    #[test]
    fn a_first_attempt_does_not_wait() {
        assert_eq!(backoff_delay(0), Duration::ZERO);
    }

    #[test]
    fn backoff_is_jittered() {
        // Identical delays across a recovering queue would re-flood the server.
        let samples: std::collections::HashSet<u128> =
            (0..40).map(|_| backoff_delay(6).as_millis()).collect();
        assert!(samples.len() > 1, "backoff must not be deterministic");
    }

    #[test]
    fn operation_ids_are_unique_and_fit_the_limit() {
        let a = new_operation_id();
        let b = new_operation_id();
        assert_ne!(a, b);
        assert!(a.len() <= crate::backup::protocol::OPERATION_ID_MAX);
    }

    #[test]
    fn pause_keeps_state_and_resume_restores_it() {
        let control = EngineControl::new();
        assert!(!control.is_paused());
        control.pause();
        assert!(control.is_paused());
        assert!(!control.is_stopped(), "pause must not stop the engine");
        control.resume();
        assert!(!control.is_paused());
    }

    #[test]
    fn stopping_is_separate_from_pausing() {
        let control = EngineControl::new();
        control.stop();
        assert!(control.is_stopped());
        assert!(!control.is_paused());
    }
}
