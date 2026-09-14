//! Backup lifecycle for one connected server.
//!
//! Everything the UI can ask for goes through here: what can be protected,
//! turning backup on, what it is doing now, pausing it, turning it off. The
//! engine underneath only drains a queue; this decides when there is one.
//!
//! The manager is also where the two credentials meet without mixing. The
//! user's session is borrowed from the WebView for the single call that
//! authorizes backup, and the `arcsync_` credential that call returns goes
//! straight into Windows Credential Manager. Neither is ever returned to
//! React, and neither is written to the sync database.

use std::path::PathBuf;
use std::sync::Arc;

use url::Url;

use crate::backup::client::{enable_backup, BackupClient, RootRequest};
use crate::backup::engine::{BackupEngine, EngineControl, EngineStatus};
use crate::backup::known_folders;
use crate::backup::protocol::{self as bp, SyncRootKind};
use crate::backup::scan::{self, CancelFlag};
use crate::backup::store::{Entry, EntryType, Root, SyncState, SyncStore};
use crate::credentials::CredentialStore;
use crate::error::{from_code, AppError};

/// A folder offered on the selection screen.
///
/// Note what is not here: the folder's path. The renderer identifies a folder
/// by `id` and nothing else, so a Windows path never crosses the IPC boundary
/// in either direction.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProtectableFolder {
    /// What the renderer passes back to select this folder.
    ///
    /// A known folder is its own kind (`DESKTOP`); a folder the person chose
    /// with the Windows picker gets an opaque `custom:<uuid>` handle that only
    /// this process can resolve.
    pub id: String,
    /// `DESKTOP`, `DOCUMENTS`, … or `CUSTOM`.
    pub kind: String,
    pub display_name: String,
    /// Suggested initial state.
    pub selected_by_default: bool,
    /// `None` while the size is still being measured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_bytes: Option<u64>,
}

/// Marks an id as referring to the custom-folder registry rather than a kind.
const CUSTOM_PREFIX: &str = "custom:";

/// Where turning backup on has got to.
///
/// # Why this is not a boolean
///
/// It used to be: the screen set `loading = true`, awaited one call that did
/// *everything*, and cleared it. That call authorises with the server, creates
/// the profile and roots, then walks every protected folder. The first part
/// takes a fifth of a second; the walk takes as long as the folders are big —
/// minutes for a real Desktop. All of it sat behind one spinner with no way to
/// tell "still working" from "wedged", and any failure inside it looked
/// identical to a hang.
///
/// Splitting it means the screen can say which part is slow, show real
/// progress once files start moving, and report a failure against the step
/// that actually failed.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "state", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ActivationStage {
    /// Nothing in flight.
    Idle,
    /// Borrowing the signed-in session to authorise backup.
    Authorizing,
    /// Asking the server to create the profile and issue a credential.
    CreatingProfile,
    /// Recording the roots the server created.
    CreatingRoots,
    /// Walking the protected folders to build the queue.
    Scanning,
    /// The queue is draining.
    Uploading,
    /// Everything is set up and the engine is running.
    Active,
    /// Activation stopped. Carries something a person can act on.
    Error { code: String, message: String },
}

impl ActivationStage {
    /// Whether an activation is in flight, so a second Start Backup is refused
    /// rather than creating a second profile.
    fn is_running(&self) -> bool {
        matches!(
            self,
            Self::Authorizing
                | Self::CreatingProfile
                | Self::CreatingRoots
                | Self::Scanning
                | Self::Uploading
        )
    }
}

/// Emitted to the onboarding window as activation moves between stages.
pub const ACTIVATION_EVENT: &str = "arciin://backup-activation";

/// A folder the person picked themselves, held only in this process.
///
/// The registry lives for as long as the app runs, which is all that is
/// needed: it exists to carry a choice from the picker to the moment backup is
/// turned on. Once a root is created, the path lives in the sync database
/// instead, because the engine has to open the files.
#[derive(Debug, Clone)]
struct CustomFolder {
    id: String,
    display_name: String,
    path: PathBuf,
}

/// One folder that is actually going to be protected.
#[derive(Debug, Clone)]
struct SelectedFolder {
    kind: SyncRootKind,
    /// What the server and the user both see. A known folder's own name, or
    /// the last path component for a custom one — never the full path.
    display_name: String,
    /// Stays local.
    path: PathBuf,
}

/// Whether backup can be offered for the connected server, and its state.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupAvailability {
    /// The server advertises `computerBackup` at a version we implement.
    pub supported: bool,
    /// This computer already has a backup profile for that server.
    pub enabled: bool,
    /// A signed-in user session was found in the WebView.
    pub signed_in: bool,
}

/// Live state for the native status surface.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupState {
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<EngineStatus>,
    pub roots: Vec<ProtectedRootView>,
    /// Present while activation is in flight or has just failed, so the screen
    /// can show the step rather than an undifferentiated spinner.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activation: Option<ActivationStage>,
    /// When a file last landed successfully, RFC 3339.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_backup_at: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProtectedRootView {
    pub id: String,
    pub kind: String,
    pub display_name: String,
    pub enabled: bool,
    /// Where the folder is on this PC.
    ///
    /// Shown in the native Backup Center, because "which Desktop?" is a real
    /// question once folders can be redirected to OneDrive or another drive.
    /// It is native-only: this field is serialized to the *onboarding* window,
    /// which is our own bundled UI. It never goes to the server, and never to
    /// the webview showing the server's page — that one has no IPC at all.
    pub local_path: String,
    pub file_count: i64,
    pub pending: i64,
    pub failed: i64,
    pub bytes_synced: i64,
}

/// Owns the engine and its control handle for one server.
struct RunningEngine {
    control: EngineControl,
}

/// Process-wide backup state.
#[derive(Default)]
pub struct BackupManager {
    store: std::sync::OnceLock<Arc<SyncStore>>,
    running: std::sync::Mutex<Option<RunningEngine>>,
    /// Cancels an in-flight folder-size measurement when the user moves on.
    sizing: std::sync::Mutex<Option<CancelFlag>>,
    /// Folders chosen with the Windows picker, by opaque handle.
    custom: std::sync::Mutex<Vec<CustomFolder>>,
    /// Where turning backup on has got to, for the UI and for refusing a
    /// second concurrent attempt.
    activation: std::sync::Mutex<Option<ActivationStage>>,
}

impl BackupManager {
    pub fn new() -> Self {
        Self::default()
    }

    fn store(&self, app: &tauri::AppHandle) -> Result<Arc<SyncStore>, AppError> {
        if let Some(store) = self.store.get() {
            return Ok(Arc::clone(store));
        }
        use tauri::Manager;
        let dir = app
            .path()
            .app_data_dir()
            .map_err(|_| AppError::internal("Backup state folder could not be found."))?;
        let store = Arc::new(SyncStore::open(&dir)?);
        let _ = self.store.set(Arc::clone(&store));
        Ok(store)
    }

    /// Known folders this machine actually has, without sizes yet.
    ///
    /// Sizes are measured separately because walking six trees can take a
    /// while and the screen should paint immediately.
    pub fn protectable_folders(&self) -> Vec<ProtectableFolder> {
        known_folders::resolve_all()
            .into_iter()
            .map(|folder| ProtectableFolder {
                id: folder.kind.as_str().to_string(),
                kind: folder.kind.as_str().to_string(),
                display_name: folder.kind.display_name().to_string(),
                selected_by_default: folder.kind.default_selected(),
                file_count: None,
                total_bytes: None,
            })
            .collect()
    }

    /// Open the Windows folder picker and register whatever comes back.
    ///
    /// Returns `None` when the person cancels. The path is kept here; what the
    /// caller gets is a handle and a display name.
    pub fn pick_custom_folder(
        &self,
        parent: Option<isize>,
    ) -> Result<Option<ProtectableFolder>, AppError> {
        let Some(path) = crate::backup::folder_picker::pick_folder(parent) else {
            return Ok(None);
        };
        self.register_custom_folder(path).map(Some)
    }

    /// Add a chosen folder to the registry, or hand back the one already there.
    ///
    /// Split out from `pick_custom_folder` so the rules can be tested without
    /// a dialog.
    fn register_custom_folder(&self, path: PathBuf) -> Result<ProtectableFolder, AppError> {
        // Between the dialog closing and this line, the folder could have been
        // moved or deleted. Better to say so now than to fail mid-scan.
        if !path.is_dir() {
            return Err(from_code("BACKUP_FOLDER_UNAVAILABLE"));
        }

        // A drive root has no file name; `D:\` is its own best label.
        let display_name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());

        let mut registry = self.custom.lock().unwrap();

        // Picking the same folder twice is a mistake, not a request for two
        // copies. Returning the existing handle means the row the user already
        // has simply stays selected.
        if let Some(existing) = registry
            .iter()
            .find(|folder| same_path(&folder.path, &path))
        {
            return Ok(custom_view(existing));
        }

        // The same guard against the known folders, which are already offered:
        // protecting Pictures twice under two names would upload it twice.
        if let Some(known) = known_folders::resolve_all()
            .into_iter()
            .find(|known| same_path(&known.path, &path))
        {
            return Err(AppError::new(
                "BACKUP_FOLDER_ALREADY_OFFERED",
                &format!(
                    "{} is already in the list above.",
                    known.kind.display_name()
                ),
                false,
            ));
        }

        let folder = CustomFolder {
            id: format!("{CUSTOM_PREFIX}{}", uuid::Uuid::new_v4()),
            display_name,
            path,
        };
        let view = custom_view(&folder);
        registry.push(folder);

        // The name is the person's own folder name, so it is logged; the path
        // it came from is not.
        tracing::info!(name = %view.display_name, "a folder was chosen with the picker");
        Ok(view)
    }

    /// Turn an id from the renderer back into a real folder.
    ///
    /// Returns `None` for anything unrecognised, so an id that never came from
    /// `protectable_folders` or the picker simply selects nothing.
    fn resolve_selection(&self, id: &str) -> Option<SelectedFolder> {
        if let Some(handle) = id.strip_prefix(CUSTOM_PREFIX) {
            let registry = self.custom.lock().unwrap();
            let folder = registry
                .iter()
                .find(|folder| folder.id.strip_prefix(CUSTOM_PREFIX) == Some(handle))?;
            // Re-checked on the way out: a folder can be deleted between being
            // picked and backup being started.
            if !folder.path.is_dir() {
                tracing::warn!(name = %folder.display_name, "a chosen folder is no longer there");
                return None;
            }
            return Some(SelectedFolder {
                kind: SyncRootKind::Custom,
                display_name: folder.display_name.clone(),
                path: folder.path.clone(),
            });
        }

        let known = parse_kind(id).and_then(known_folders::resolve)?;
        Some(SelectedFolder {
            kind: known.kind,
            display_name: known.kind.display_name().to_string(),
            path: known.path,
        })
    }

    /// Measure one folder. Cancels any previous measurement first.
    ///
    /// Runs on a blocking thread: a deep tree is IO-bound and would otherwise
    /// stall the async runtime the UI depends on.
    pub async fn measure_folder(&self, id: &str) -> Result<scan::ScanSummary, AppError> {
        let Some(folder) = self.resolve_selection(id) else {
            return Err(from_code("BACKUP_FOLDER_UNAVAILABLE"));
        };

        let cancel = CancelFlag::new();
        {
            let mut slot = self.sizing.lock().unwrap();
            if let Some(previous) = slot.replace(cancel.clone()) {
                previous.cancel();
            }
        }

        let path = folder.path.clone();
        let flag = cancel.clone();
        let summary = tokio::task::spawn_blocking(move || scan::summarize_root(&path, &flag))
            .await
            .map_err(|_| AppError::internal("Measuring that folder was interrupted."))?;

        tracing::info!(
            kind = folder.kind.as_str(),
            file_count = summary.file_count,
            total_bytes = summary.total_bytes,
            "measured a protectable folder"
        );
        Ok(summary)
    }

    /// Stop any running size measurement.
    pub fn cancel_measuring(&self) {
        if let Some(cancel) = self.sizing.lock().unwrap().take() {
            cancel.cancel();
        }
    }

    /// Can backup be offered here, and is it already on?
    pub fn availability(
        &self,
        app: &tauri::AppHandle,
        server_id: &str,
        server_supports_backup: bool,
        signed_in: bool,
    ) -> Result<BackupAvailability, AppError> {
        let enabled = self.store(app)?.profile(server_id)?.is_some();
        Ok(BackupAvailability {
            supported: server_supports_backup,
            enabled,
            signed_in,
        })
    }

    /// Turn backup on for the chosen folders.
    ///
    /// The order matters: the profile is created first (that is what issues the
    /// credential), the credential is stored before anything else can fail, and
    /// only then is the local queue seeded. If seeding is interrupted, the
    /// profile and credential survive and the next launch simply rescans.
    #[allow(clippy::too_many_arguments)]
    pub async fn enable(
        &self,
        app: &tauri::AppHandle,
        credentials: &dyn CredentialStore,
        server_id: &str,
        origin: &Url,
        device_id: &str,
        session_cookie_header: &str,
        selections: &[String],
    ) -> Result<BackupState, AppError> {
        // A second Start Backup while the first is still running would create
        // a second profile on the server. Refused rather than queued: the
        // first attempt is already reporting its own progress.
        {
            let mut slot = self.activation.lock().unwrap();
            if slot.as_ref().is_some_and(ActivationStage::is_running) {
                return Err(from_code("BACKUP_ALREADY_STARTING"));
            }
            *slot = Some(ActivationStage::Authorizing);
        }

        let store = self.store(app)?;

        // Resolve every requested folder locally before telling the server
        // about any of them.
        let mut folders: Vec<SelectedFolder> = Vec::new();
        for id in selections {
            let Some(folder) = self.resolve_selection(id) else {
                // Logged by kind, never by id: a custom id is a handle to a
                // path, and the path is the part worth not writing down.
                tracing::warn!("a requested folder is not available on this machine");
                continue;
            };
            folders.push(folder);
        }
        if folders.is_empty() {
            return Err(from_code("BACKUP_NO_FOLDERS"));
        }

        // Pair each folder with the identifier the server will know it by, and
        // drop repeats: the same folder reached twice (a known folder and a
        // picked one, say) is one root, and the server would reject the second
        // against its own uniqueness constraint anyway.
        let mut requests: Vec<RootRequest> = Vec::new();
        let mut unique: Vec<SelectedFolder> = Vec::new();
        for folder in folders {
            let source_path_identifier =
                known_folders::source_path_identifier(server_id, folder.kind, &folder.path);
            if requests
                .iter()
                .any(|existing| existing.source_path_identifier == source_path_identifier)
            {
                continue;
            }
            requests.push(RootRequest {
                kind: folder.kind.as_str().to_string(),
                display_name: folder.display_name.clone(),
                source_path_identifier,
            });
            unique.push(folder);
        }
        let folders = unique;

        self.set_stage(app, ActivationStage::CreatingProfile);
        let outcome = match enable_backup(origin, session_cookie_header, device_id, &requests).await
        {
            Ok(outcome) => outcome,
            Err(err) => {
                self.fail(app, &err);
                return Err(err);
            }
        };
        let profile = outcome.profile.clone();

        // The one and only place the sync credential is touched above the
        // transport layer. Stored before anything else can go wrong.
        match outcome.into_credential() {
            Some(secret) => credentials.save_sync(server_id, &profile.id, &secret)?,
            None => {
                // The server already had a grant and did not reissue. Whatever
                // is in the store must still be valid, or the user has to
                // rotate — either way there is nothing to save.
                if credentials.load_sync(server_id, &profile.id)?.is_none() {
                    // The server keeps the profile when backup is stopped
                    // locally, and only issues a credential when it creates
                    // one. So a computer that stopped backup and deleted its
                    // credential cannot talk its way back in from here — the
                    // grant has to be reset on the server.
                    let err = from_code("BACKUP_CREDENTIAL_INVALID");
                    self.fail(app, &err);
                    return Err(err);
                }
            }
        }

        store.save_profile(&crate::backup::store::Profile {
            server_id: server_id.to_string(),
            profile_id: profile.id.clone(),
            device_id: profile.device_id.clone(),
            user_id: profile.user_id.clone(),
            paused: false,
        })?;

        self.set_stage(app, ActivationStage::CreatingRoots);

        // Map the server's roots back to the local folders they came from.
        //
        // Matched on the identifier rather than the kind: several roots can be
        // `CUSTOM`, so kind alone would tie them all to whichever folder came
        // first and the engine would upload one tree repeatedly.
        for server_root in &profile.roots {
            let Some(folder) = requests
                .iter()
                .position(|request| {
                    request.source_path_identifier == server_root.source_path_identifier
                })
                .and_then(|index| folders.get(index))
            else {
                continue;
            };
            store.save_root(
                server_id,
                &Root {
                    id: server_root.id.clone(),
                    kind: server_root.kind.clone(),
                    display_name: server_root.display_name.clone(),
                    local_path: folder.path.clone(),
                    enabled: true,
                },
            )?;
        }

        tracing::info!(
            server_id,
            profile_id = %profile.id,
            root_count = profile.roots.len(),
            "computer backup enabled for this machine"
        );

        // Everything above is the part that had to happen before this call can
        // be called a success: the server has the profile and the roots, and
        // the credential is in the OS store. It takes a fraction of a second.
        //
        // What follows — walking the folders and draining the queue — is
        // open-ended, and used to run inside this same call. That is what made
        // Start Backup look wedged: a real Desktop takes minutes to walk, and
        // the screen had nothing to show for it. It now runs behind the
        // activation stage, so the screen leaves the spinner immediately and
        // switches to real progress.
        self.spawn_activation(app, server_id, origin);
        self.state(app, server_id)
    }

    /// Finish activation in the background: scan, then run the engine.
    ///
    /// Split from `enable` so the command can return as soon as the server
    /// state exists. Every exit reports a stage, so the UI can never be left
    /// waiting on something that already stopped.
    fn spawn_activation(&self, app: &tauri::AppHandle, server_id: &str, origin: &Url) {
        let handle = app.clone();
        let server = server_id.to_string();
        let origin = origin.clone();

        tauri::async_runtime::spawn(async move {
            let Some(state) = tauri::Manager::try_state::<crate::AppState>(&handle) else {
                return;
            };
            let manager = &state.backup;
            let credentials = Arc::clone(&state.credentials);

            manager.set_stage(&handle, ActivationStage::Scanning);
            let store = match manager.store(&handle) {
                Ok(store) => store,
                Err(err) => return manager.fail(&handle, &err),
            };
            if let Err(err) = manager.seed_queue(&store, &server).await {
                return manager.fail(&handle, &err);
            }

            manager.set_stage(&handle, ActivationStage::Uploading);
            if let Err(err) = manager
                .start(&handle, credentials.as_ref(), &server, &origin)
                .await
            {
                return manager.fail(&handle, &err);
            }

            // The engine is running; from here the queue counters are the
            // honest progress report, so activation itself is done.
            manager.set_stage(&handle, ActivationStage::Active);

            // The server's page still believes this computer has no backup.
            // Refresh it once, and only if that is what the person is looking
            // at. This is the one place it happens, so it cannot loop.
            refresh_computers_page(&handle);
        });
    }

    /// Record a stage and tell the UI about it.
    fn set_stage(&self, app: &tauri::AppHandle, stage: ActivationStage) {
        use tauri::Emitter;
        *self.activation.lock().unwrap() = Some(stage.clone());
        if let Some(window) =
            tauri::Manager::get_webview_window(app, crate::connection::ONBOARDING_WINDOW)
        {
            let _ = window.emit(ACTIVATION_EVENT, &stage);
        }
    }

    /// Stop activation with something the person can act on.
    fn fail(&self, app: &tauri::AppHandle, err: &AppError) {
        tracing::warn!(code = %err.code, "computer backup activation failed");
        self.set_stage(
            app,
            ActivationStage::Error {
                code: err.code.clone(),
                message: err.message.clone(),
            },
        );
    }

    /// The current activation stage, if there is one worth showing.
    fn stage(&self) -> Option<ActivationStage> {
        self.activation.lock().unwrap().clone()
    }

    /// Resume backup for a server that already has a profile.
    ///
    /// Called after connecting. Without it the engine only ever ran in the
    /// session that turned backup on: every later launch left the queue
    /// sitting there, uploading nothing, while the UI happily reported the
    /// counts from the database.
    ///
    /// Safe to call repeatedly. It creates nothing — no profile, no root, no
    /// credential — so an interrupted first activation resumes here rather
    /// than starting a second one.
    pub async fn resume_existing(
        &self,
        app: &tauri::AppHandle,
        credentials: &dyn CredentialStore,
        server_id: &str,
        origin: &Url,
    ) -> Result<(), AppError> {
        let store = self.store(app)?;
        let Some(profile) = store.profile(server_id)? else {
            return Ok(());
        };

        tracing::info!(
            server_id,
            profile_id = %profile.profile_id,
            "resuming computer backup for a profile that already exists"
        );

        // Rescan first: a launch that was interrupted mid-scan has a partial
        // queue, and files change while the app is closed. `record` keeps the
        // identity of anything already sent and skips what is unchanged, so
        // this costs a walk and re-uploads nothing.
        self.set_stage(app, ActivationStage::Scanning);
        if let Err(err) = self.seed_queue(&store, server_id).await {
            self.fail(app, &err);
            return Err(err);
        }

        self.set_stage(app, ActivationStage::Uploading);
        if let Err(err) = self.start(app, credentials, server_id, origin).await {
            self.fail(app, &err);
            return Err(err);
        }

        self.set_stage(app, ActivationStage::Active);
        Ok(())
    }

    /// Walk every enabled root and record what is there.
    ///
    /// Existing entries keep their `clientEntryId`, so a rescan after a restart
    /// recognises what it already sent instead of uploading it twice.
    async fn seed_queue(&self, store: &Arc<SyncStore>, server_id: &str) -> Result<(), AppError> {
        let roots = store.roots(server_id)?;
        for root in roots.iter().filter(|r| r.enabled) {
            let path = root.local_path.clone();
            let cancel = CancelFlag::new();
            let result = tokio::task::spawn_blocking(move || {
                scan::scan_root(&path, &cancel, &scan::ScanProgress::default())
            })
            .await
            .map_err(|_| AppError::internal("Scanning a protected folder was interrupted."))?;

            // Folders first: the tree must exist before files land in it.
            for folder in &result.folders {
                self.record(
                    store,
                    server_id,
                    &root.id,
                    &folder.relative_path,
                    EntryType::Folder,
                    0,
                    0,
                )?;
            }
            for file in &result.files {
                self.record(
                    store,
                    server_id,
                    &root.id,
                    &file.relative_path,
                    EntryType::File,
                    file.size_bytes as i64,
                    file.modified_ms,
                )?;
            }

            tracing::info!(
                server_id,
                root_id = %root.id,
                folders = result.folders.len(),
                files = result.files.len(),
                bytes = result.total_bytes,
                skipped_reparse = result.skipped_reparse,
                skipped_unreadable = result.skipped_unreadable,
                "protected folder scanned"
            );
        }
        Ok(())
    }

    /// Insert or refresh one entry, preserving its identity if already known.
    #[allow(clippy::too_many_arguments)]
    fn record(
        &self,
        store: &Arc<SyncStore>,
        server_id: &str,
        root_id: &str,
        relative_path: &str,
        entry_type: EntryType,
        size_bytes: i64,
        modified_ms: i64,
    ) -> Result<(), AppError> {
        let existing = store.entry_by_path(server_id, root_id, relative_path)?;
        let (client_entry_id, state) = match &existing {
            // Unchanged since the last successful send: leave it alone rather
            // than re-queueing bytes the server already has.
            Some(entry)
                if entry.state == SyncState::Synced
                    && entry.size_bytes == size_bytes
                    && entry.modified_ms == modified_ms =>
            {
                return Ok(())
            }
            Some(entry) => (entry.client_entry_id.clone(), SyncState::Pending),
            None => (uuid::Uuid::new_v4().to_string(), SyncState::Pending),
        };

        store.upsert_entry(
            server_id,
            &Entry {
                client_entry_id,
                root_id: root_id.to_string(),
                relative_path: relative_path.to_string(),
                entry_type,
                size_bytes,
                modified_ms,
                state,
                pending_operation_id: existing.and_then(|e| e.pending_operation_id),
            },
        )
    }

    /// Start (or restart) the engine for a server whose credential is stored.
    pub async fn start(
        &self,
        app: &tauri::AppHandle,
        credentials: &dyn CredentialStore,
        server_id: &str,
        origin: &Url,
    ) -> Result<(), AppError> {
        let store = self.store(app)?;
        let Some(profile) = store.profile(server_id)? else {
            return Ok(());
        };
        let Some(credential) = credentials.load_sync(server_id, &profile.profile_id)? else {
            tracing::warn!(server_id, "backup is configured but its credential is gone");
            return Err(from_code("BACKUP_CREDENTIAL_INVALID"));
        };

        self.stop();

        let client = Arc::new(BackupClient::new(origin.clone(), credential));

        // Confirm the grant before starting: a revoked or disabled profile is
        // then one clean error instead of a storm of rejected uploads.
        client.me().await?;

        let control = EngineControl::new();
        if profile.paused {
            control.pause();
        }
        *self.running.lock().unwrap() = Some(RunningEngine {
            control: control.clone(),
        });

        let engine = BackupEngine::new(server_id.to_string(), client, Arc::clone(&store), control);
        let server = server_id.to_string();
        let handle = app.clone();
        tokio::spawn(async move {
            let Err(err) = engine.run().await else { return };
            tracing::warn!(server_id = %server, code = %err.code, "backup engine ended with an error");

            // The engine is usually the first thing to notice a revocation,
            // because it is the only part making requests continuously. Two
            // outcomes, deliberately different:
            //
            //   device revoked  -> the computer is no longer paired at all
            //   backup disabled -> the computer stays paired, backup stops
            if crate::connection::trust::report_failure(&handle, &server, &err) {
                return;
            }
            if matches!(
                err.code.as_str(),
                "BACKUP_DISABLED" | "BACKUP_CREDENTIAL_INVALID"
            ) {
                if let Some(state) = tauri::Manager::try_state::<crate::AppState>(&handle) {
                    // Drop the grant and its local queue, keep the pairing.
                    if let Err(cleanup) =
                        state
                            .backup
                            .forget(&handle, state.credentials.as_ref(), &server)
                    {
                        tracing::warn!(code = %cleanup.code, "backup state could not be cleared");
                    }
                    tracing::info!(
                        server_id = %server,
                        "computer backup was turned off server-side; this computer stays paired"
                    );
                }
            }
        });
        Ok(())
    }

    pub fn stop(&self) {
        if let Some(running) = self.running.lock().unwrap().take() {
            running.control.stop();
        }
    }

    pub fn pause(&self, app: &tauri::AppHandle, server_id: &str) -> Result<(), AppError> {
        if let Some(running) = self.running.lock().unwrap().as_ref() {
            running.control.pause();
        }
        self.store(app)?.set_paused(server_id, true)
    }

    pub fn resume(&self, app: &tauri::AppHandle, server_id: &str) -> Result<(), AppError> {
        if let Some(running) = self.running.lock().unwrap().as_ref() {
            running.control.resume();
        }
        self.store(app)?.set_paused(server_id, false)
    }

    /// Current state for the UI.
    pub fn state(&self, app: &tauri::AppHandle, server_id: &str) -> Result<BackupState, AppError> {
        let store = self.store(app)?;
        let Some(profile) = store.profile(server_id)? else {
            return Ok(BackupState {
                enabled: false,
                status: None,
                roots: Vec::new(),
                activation: self.stage(),
                last_backup_at: None,
            });
        };

        let (synced, outstanding, failed, bytes, pending_bytes) = store.progress(server_id)?;
        let paused = profile.paused;
        let health = if paused {
            bp::BackupHealth::Paused
        } else if outstanding > 0 {
            bp::BackupHealth::Syncing
        } else {
            bp::BackupHealth::UpToDate
        };

        Ok(BackupState {
            enabled: true,
            activation: self.stage(),
            last_backup_at: store.last_synced_at(server_id)?,
            status: Some(EngineStatus {
                health: health.as_str().to_string(),
                files_synced: synced,
                files_outstanding: outstanding,
                files_failed: failed,
                bytes_synced: bytes,
                bytes_outstanding: pending_bytes,
                paused,
                last_error: None,
            }),
            roots: store
                .roots(server_id)?
                .into_iter()
                .map(|root| {
                    let (files, pending, failed, bytes) = store
                        .root_progress(server_id, &root.id)
                        .unwrap_or((0, 0, 0, 0));
                    ProtectedRootView {
                        local_path: root.local_path.to_string_lossy().into_owned(),
                        id: root.id,
                        kind: root.kind,
                        display_name: root.display_name,
                        enabled: root.enabled,
                        file_count: files,
                        pending,
                        failed,
                        bytes_synced: bytes,
                    }
                })
                .collect(),
        })
    }

    /// Stop protecting one folder.
    ///
    /// Deliberately narrow: the engine keeps running, the profile and every
    /// other root stay, the Device stays paired, the copy already on the
    /// server stays, and **nothing on this PC is touched**. Removing a folder
    /// from backup is not a request to delete it in either place.
    pub fn remove_root(
        &self,
        app: &tauri::AppHandle,
        server_id: &str,
        root_id: &str,
    ) -> Result<BackupState, AppError> {
        let store = self.store(app)?;
        store.disable_root(server_id, root_id)?;
        tracing::info!(server_id, root_id, "a folder is no longer being backed up");
        self.state(app, server_id)
    }

    /// Where one protected root lives on this PC, for opening it in Explorer.
    pub fn root_path(
        &self,
        app: &tauri::AppHandle,
        server_id: &str,
        root_id: &str,
    ) -> Result<PathBuf, AppError> {
        let store = self.store(app)?;
        store
            .roots(server_id)?
            .into_iter()
            .find(|root| root.id == root_id)
            .map(|root| root.local_path)
            .ok_or_else(|| from_code("BACKUP_FOLDER_UNAVAILABLE"))
    }

    /// Turn backup off locally: stop the engine, drop the sync credential,
    /// forget the local queue.
    ///
    /// Local files are never touched, and pairing is left completely alone —
    /// the computer stays a trusted device.
    pub fn forget(
        &self,
        app: &tauri::AppHandle,
        credentials: &dyn CredentialStore,
        server_id: &str,
    ) -> Result<(), AppError> {
        self.stop();
        *self.activation.lock().unwrap() = None;
        let store = self.store(app)?;
        if let Some(profile) = store.profile(server_id)? {
            credentials.delete_sync(server_id, &profile.profile_id)?;
        }
        store.forget_server(server_id)?;
        tracing::info!(server_id, "computer backup turned off on this machine");
        Ok(())
    }
}

/// The page that goes stale the moment backup is turned on.
const COMPUTERS_PATH: &str = "/computers";

/// Ask the Arciin window to reload My Computers, once, if it is showing it.
///
/// Called at exactly one point — the transition to `Active` — so there is no
/// polling and no possibility of a reload loop. Anyone on a different page is
/// left where they are.
fn refresh_computers_page(app: &tauri::AppHandle) {
    use tauri::Manager;

    let Some(state) = app.try_state::<crate::AppState>() else {
        return;
    };
    let Some(content) = state.content_webview.lock().unwrap().clone() else {
        tracing::info!("arciin window is not open; nothing to refresh");
        return;
    };

    match crate::connection::webview::reload_if_showing(&content, COMPUTERS_PATH) {
        Ok(true) => tracing::info!("refreshed my computers after backup activation"),
        Ok(false) => {
            tracing::info!("arciin is not on my computers; leaving the page alone")
        }
        Err(err) => tracing::info!(code = %err.code, "could not refresh my computers"),
    }
}

/// The view of a custom folder that may cross the IPC boundary.
fn custom_view(folder: &CustomFolder) -> ProtectableFolder {
    ProtectableFolder {
        id: folder.id.clone(),
        kind: SyncRootKind::Custom.as_str().to_string(),
        display_name: folder.display_name.clone(),
        // Someone who just picked a folder means to protect it.
        selected_by_default: true,
        file_count: None,
        total_bytes: None,
    }
}

/// Whether two paths name the same folder on Windows.
///
/// Windows paths are case-insensitive, and a trailing separator is not part of
/// the name — `C:\Work` and `c:\work\` are one folder, and offering it twice
/// would back it up twice.
fn same_path(left: &std::path::Path, right: &std::path::Path) -> bool {
    fn normalise(path: &std::path::Path) -> String {
        let text = path.to_string_lossy().to_lowercase().replace('/', "\\");
        // A drive root is just `c:\`; anything else keeps its last component.
        let trimmed = text.trim_end_matches('\\');
        if trimmed.len() < text.len() && trimmed.ends_with(':') {
            text
        } else {
            trimmed.to_string()
        }
    }
    normalise(left) == normalise(right)
}

fn parse_kind(raw: &str) -> Option<SyncRootKind> {
    Some(match raw.to_ascii_uppercase().as_str() {
        "DESKTOP" => SyncRootKind::Desktop,
        "DOCUMENTS" => SyncRootKind::Documents,
        "PICTURES" => SyncRootKind::Pictures,
        "VIDEOS" => SyncRootKind::Videos,
        "MUSIC" => SyncRootKind::Music,
        "DOWNLOADS" => SyncRootKind::Downloads,
        _ => return None,
    })
}

/// Where a root's files live locally. Used by the engine to open them.
pub fn root_path(root: &Root) -> PathBuf {
    root.local_path.clone()
}

#[cfg(test)]
mod activation_tests {
    use super::*;

    #[test]
    fn only_in_flight_stages_block_a_second_start() {
        // The guard that stops a double click creating two profiles.
        for stage in [
            ActivationStage::Authorizing,
            ActivationStage::CreatingProfile,
            ActivationStage::CreatingRoots,
            ActivationStage::Scanning,
            ActivationStage::Uploading,
        ] {
            assert!(stage.is_running(), "{stage:?} must block a second start");
        }
    }

    #[test]
    fn a_finished_activation_does_not_block_a_retry() {
        // Otherwise Try Again would be dead after the first failure, and the
        // screen would be stuck in exactly the way this work is fixing.
        assert!(!ActivationStage::Idle.is_running());
        assert!(!ActivationStage::Active.is_running());
        assert!(!ActivationStage::Error {
            code: "UNREACHABLE".into(),
            message: "The server could not be reached.".into(),
        }
        .is_running());
    }

    #[test]
    fn an_error_stage_carries_something_actionable() {
        // A stage the UI can render as "couldn't start / <reason> / Try Again"
        // rather than a spinner that never resolves.
        let stage = ActivationStage::Error {
            code: "BACKUP_CREDENTIAL_INVALID".into(),
            message: "Backup needs to be reset on the server.".into(),
        };
        let wire = serde_json::to_string(&stage).unwrap();
        assert!(wire.contains("\"state\":\"ERROR\""));
        assert!(wire.contains("BACKUP_CREDENTIAL_INVALID"));
    }

    #[test]
    fn stages_serialize_as_the_renderer_expects() {
        // The tag the TypeScript union switches on.
        for (stage, expected) in [
            (ActivationStage::Idle, "IDLE"),
            (ActivationStage::Authorizing, "AUTHORIZING"),
            (ActivationStage::CreatingProfile, "CREATING_PROFILE"),
            (ActivationStage::CreatingRoots, "CREATING_ROOTS"),
            (ActivationStage::Scanning, "SCANNING"),
            (ActivationStage::Uploading, "UPLOADING"),
            (ActivationStage::Active, "ACTIVE"),
        ] {
            let wire = serde_json::to_string(&stage).unwrap();
            assert_eq!(wire, format!("{{\"state\":\"{expected}\"}}"));
        }
    }

    #[test]
    fn a_second_start_is_refused_while_one_is_running() {
        let manager = BackupManager::new();
        *manager.activation.lock().unwrap() = Some(ActivationStage::Scanning);

        let blocked = manager
            .activation
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(ActivationStage::is_running);
        assert!(
            blocked,
            "a duplicate Start Backup must not create a profile"
        );
    }

    #[test]
    fn turning_backup_off_clears_the_stage() {
        // Otherwise a stale ERROR would greet the next setup attempt.
        let manager = BackupManager::new();
        *manager.activation.lock().unwrap() = Some(ActivationStage::Error {
            code: "UNREACHABLE".into(),
            message: "no".into(),
        });
        *manager.activation.lock().unwrap() = None;
        assert!(manager.stage().is_none());
    }

    #[test]
    fn state_reports_the_stage_so_the_ui_never_guesses() {
        let manager = BackupManager::new();
        assert!(manager.stage().is_none());
        *manager.activation.lock().unwrap() = Some(ActivationStage::Uploading);
        assert_eq!(manager.stage(), Some(ActivationStage::Uploading));
    }
}

#[cfg(test)]
mod custom_folder_tests {
    use super::*;

    const SERVER_A: &str = "3f2504e0-4f89-41d3-9a0c-0305e82c3301";
    const SERVER_B: &str = "9c858901-8a57-4791-81fe-4c455b099bc9";

    /// A real directory to stand in for one the picker returned.
    fn folder(name: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        std::fs::create_dir(&path).unwrap();
        (dir, path)
    }

    fn profile(server_id: &str) -> crate::backup::store::Profile {
        crate::backup::store::Profile {
            server_id: server_id.into(),
            profile_id: "profile-1".into(),
            device_id: "device-1".into(),
            user_id: "user-1".into(),
            paused: false,
        }
    }

    #[test]
    fn a_picked_folder_becomes_a_custom_root() {
        let (_guard, path) = folder("TestBackup");
        let manager = BackupManager::new();

        let view = manager.register_custom_folder(path.clone()).unwrap();

        assert_eq!(view.kind, "CUSTOM");
        // The folder's own name, not the path it sits at.
        assert_eq!(view.display_name, "TestBackup");
        assert!(view.id.starts_with(CUSTOM_PREFIX));
        // Someone who just picked a folder means to protect it.
        assert!(view.selected_by_default);

        let resolved = manager.resolve_selection(&view.id).unwrap();
        assert_eq!(resolved.kind, SyncRootKind::Custom);
        assert_eq!(resolved.display_name, "TestBackup");
        assert_eq!(resolved.path, path);
    }

    #[test]
    fn the_handle_given_to_the_renderer_carries_no_path() {
        // The rule this whole indirection exists for: a Windows path names the
        // account, and often a client or a project, so it must not cross IPC.
        let (_guard, path) = folder("Invoices");
        let manager = BackupManager::new();
        let view = manager.register_custom_folder(path.clone()).unwrap();

        let wire = serde_json::to_string(&view).unwrap().to_lowercase();
        assert!(
            !wire.contains('\\'),
            "a path separator reached the renderer"
        );
        assert!(!wire.contains('/'));

        for component in path.iter() {
            let part = component.to_string_lossy().to_lowercase();
            // The leaf *is* the display name; every parent must be absent.
            if part == "invoices" {
                continue;
            }
            assert!(!wire.contains(&part), "{part} reached the renderer");
        }
    }

    #[test]
    fn an_unknown_handle_selects_nothing() {
        // The only ids that resolve are ones this process minted, so a guessed
        // or injected one picks no folder at all.
        let manager = BackupManager::new();
        for id in [
            "custom:00000000-0000-0000-0000-000000000000",
            r"custom:C:\Users\TestUser",
            r"C:\Users\TestUser\Desktop",
            "custom:",
            "CUSTOM",
            "",
        ] {
            assert!(
                manager.resolve_selection(id).is_none(),
                "{id} must not resolve"
            );
        }
    }

    #[test]
    fn picking_the_same_folder_twice_adds_one_row() {
        let (_guard, path) = folder("TestBackup");
        let manager = BackupManager::new();

        let first = manager.register_custom_folder(path.clone()).unwrap();
        let second = manager.register_custom_folder(path).unwrap();

        assert_eq!(first.id, second.id, "the same folder must reuse its handle");
        assert_eq!(manager.custom.lock().unwrap().len(), 1);
    }

    #[test]
    fn the_same_folder_in_a_different_case_is_still_the_same_folder() {
        // Windows paths are case-insensitive, so this would otherwise become a
        // second root uploading an identical tree.
        let (_guard, path) = folder("TestBackup");
        let manager = BackupManager::new();

        let first = manager.register_custom_folder(path.clone()).unwrap();
        let shouted = PathBuf::from(path.to_string_lossy().to_uppercase());
        let second = manager.register_custom_folder(shouted).unwrap();

        assert_eq!(first.id, second.id);
        assert_eq!(manager.custom.lock().unwrap().len(), 1);
    }

    #[test]
    fn several_different_folders_each_get_their_own_handle() {
        let dir = tempfile::tempdir().unwrap();
        let manager = BackupManager::new();

        let mut ids = Vec::new();
        for name in ["TestBackup", "WebProject", "Invoices"] {
            let path = dir.path().join(name);
            std::fs::create_dir(&path).unwrap();
            let view = manager.register_custom_folder(path).unwrap();
            assert_eq!(view.display_name, name);
            ids.push(view.id);
        }

        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 3, "three folders must be three roots");
    }

    #[test]
    fn a_folder_that_is_not_there_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let manager = BackupManager::new();

        let missing = dir.path().join("NeverExisted");
        assert!(manager.register_custom_folder(missing).is_err());

        // A file is not a folder either.
        let file = dir.path().join("notes.txt");
        std::fs::write(&file, b"x").unwrap();
        assert!(manager.register_custom_folder(file).is_err());
    }

    #[test]
    fn a_folder_deleted_after_being_picked_stops_resolving() {
        // The gap between choosing a folder and pressing Start Backup is long
        // enough for it to be moved or deleted.
        let (_guard, path) = folder("TestBackup");
        let manager = BackupManager::new();
        let view = manager.register_custom_folder(path.clone()).unwrap();

        assert!(manager.resolve_selection(&view.id).is_some());
        std::fs::remove_dir(&path).unwrap();
        assert!(
            manager.resolve_selection(&view.id).is_none(),
            "a folder that is gone must not be scanned"
        );
    }

    #[test]
    fn cancelling_the_picker_changes_nothing() {
        // Cancelling is modelled exactly as "no path came back", which is what
        // `folder_picker::pick_folder` returns when the dialog is dismissed.
        let manager = BackupManager::new();
        let cancelled: Option<PathBuf> = None;

        let outcome = match cancelled {
            Some(path) => manager.register_custom_folder(path).map(Some),
            None => Ok(None),
        };

        assert!(outcome.unwrap().is_none());
        assert!(manager.custom.lock().unwrap().is_empty());
    }

    #[test]
    fn a_custom_root_identifier_is_stable_and_scoped_to_one_server() {
        let (_guard, path) = folder("TestBackup");

        let first = known_folders::source_path_identifier(SERVER_A, SyncRootKind::Custom, &path);
        let again = known_folders::source_path_identifier(SERVER_A, SyncRootKind::Custom, &path);
        assert_eq!(first, again, "the same folder must keep its identifier");

        // Two Arciin servers must not be able to tell they are looking at the
        // same machine by comparing identifiers.
        let elsewhere =
            known_folders::source_path_identifier(SERVER_B, SyncRootKind::Custom, &path);
        assert_ne!(first, elsewhere);
    }

    #[test]
    fn a_custom_root_identifier_never_contains_the_path() {
        let (_guard, path) = folder("TestBackup");
        let id = known_folders::source_path_identifier(SERVER_A, SyncRootKind::Custom, &path);

        assert!(!id.to_lowercase().contains("testbackup"));
        // The server rejects an identifier that looks like a path at all.
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn two_custom_roots_keep_separate_identifiers() {
        // Every CUSTOM root shares a kind, so the identifier is the only thing
        // telling them apart when the server's roots are mapped back to local
        // folders. Matching on kind would tie both to whichever came first.
        let dir = tempfile::tempdir().unwrap();
        let one = dir.path().join("TestBackup");
        let two = dir.path().join("WebProject");
        std::fs::create_dir(&one).unwrap();
        std::fs::create_dir(&two).unwrap();

        assert_ne!(
            known_folders::source_path_identifier(SERVER_A, SyncRootKind::Custom, &one),
            known_folders::source_path_identifier(SERVER_A, SyncRootKind::Custom, &two),
        );
    }

    #[test]
    fn a_custom_roots_local_path_survives_a_restart() {
        // The path leaves the registry once the root exists; from then on the
        // sync database is what lets the engine find the files again.
        let (_guard, path) = folder("TestBackup");
        let dir = tempfile::tempdir().unwrap();
        let store = SyncStore::open(dir.path()).unwrap();

        store.save_profile(&profile(SERVER_A)).unwrap();
        store
            .save_root(
                SERVER_A,
                &Root {
                    id: "root-1".into(),
                    kind: "CUSTOM".into(),
                    display_name: "TestBackup".into(),
                    local_path: path.clone(),
                    enabled: true,
                },
            )
            .unwrap();
        drop(store);

        let reopened = SyncStore::open(dir.path()).unwrap();
        let roots = reopened.roots(SERVER_A).unwrap();
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].kind, "CUSTOM");
        assert_eq!(roots[0].display_name, "TestBackup");
        assert_eq!(root_path(&roots[0]), path);
    }

    #[test]
    fn one_servers_custom_roots_are_invisible_to_another() {
        let (_guard, path) = folder("TestBackup");
        let dir = tempfile::tempdir().unwrap();
        let store = SyncStore::open(dir.path()).unwrap();

        store.save_profile(&profile(SERVER_A)).unwrap();
        store
            .save_root(
                SERVER_A,
                &Root {
                    id: "root-1".into(),
                    kind: "CUSTOM".into(),
                    display_name: "TestBackup".into(),
                    local_path: path,
                    enabled: true,
                },
            )
            .unwrap();

        assert_eq!(store.roots(SERVER_A).unwrap().len(), 1);
        assert!(
            store.roots(SERVER_B).unwrap().is_empty(),
            "another server must not see this one's roots"
        );
    }

    #[test]
    fn windows_paths_compare_as_windows_paths() {
        use std::path::Path;
        assert!(same_path(Path::new(r"C:\Work"), Path::new(r"c:\work")));
        assert!(same_path(Path::new(r"C:\Work"), Path::new(r"C:\Work\")));
        assert!(same_path(Path::new(r"C:\"), Path::new(r"C:\")));
        assert!(!same_path(Path::new(r"C:\Work"), Path::new(r"C:\Work2")));
        assert!(!same_path(Path::new(r"C:\Work"), Path::new(r"D:\Work")));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_offered_kind_round_trips() {
        for kind in SyncRootKind::known_folders() {
            assert_eq!(parse_kind(kind.as_str()), Some(kind));
        }
    }

    #[test]
    fn kind_parsing_is_case_insensitive() {
        assert_eq!(parse_kind("desktop"), Some(SyncRootKind::Desktop));
        assert_eq!(parse_kind("Documents"), Some(SyncRootKind::Documents));
    }

    #[test]
    fn unknown_kinds_are_refused() {
        // A custom root is not offered in V1, and anything else is a mistake.
        assert_eq!(parse_kind("CUSTOM"), None);
        assert_eq!(parse_kind("C:/Users/TestUser"), None);
        assert_eq!(parse_kind(""), None);
    }
}
