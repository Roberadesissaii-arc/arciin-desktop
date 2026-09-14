//! The IPC surface.
//!
//! This is the only place React can reach native code, and it is deliberately
//! narrow. Note what is *absent*: there is no command that returns a device
//! credential, and no command that accepts one. React can ask for a server to
//! be paired or connected; it can never hold the secret that makes either work.

use tauri::{AppHandle, Manager, State};

use crate::address::candidate_origins;
use crate::connection::{self, ARCIIN_WINDOW};
use crate::discovery::{self, VerifiedServer};
use crate::error::{from_code, AppError};
use crate::pairing;
use crate::servers::{SavedServer, ServerStore};
use crate::AppState;

/// What the UI learns after a successful pairing.
///
/// Note the shape: `paired: true` and the device's display name. The
/// credential is not here, and there is no field it could hide in.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairResult {
    pub paired: bool,
    pub server_id: String,
    pub device_name: String,
}

/// Browse the LAN for Arciin servers.
///
/// Never fails because of mDNS: an empty list is a normal answer and the UI
/// offers the manual address path either way.
#[tauri::command]
pub async fn discover_servers() -> Result<Vec<VerifiedServer>, AppError> {
    discovery::discover().await
}

/// Verify a typed address and return the server behind it.
#[tauri::command]
pub async fn verify_server(address: String) -> Result<VerifiedServer, AppError> {
    discovery::verify_address(&address).await
}

/// The servers this computer already knows about.
#[tauri::command]
pub fn saved_servers(app: AppHandle) -> Result<Vec<SavedServer>, AppError> {
    Ok(ServerStore::new(&app)?.list())
}

/// The default name this computer will offer when pairing.
#[tauri::command]
pub fn suggested_device_name() -> String {
    pairing::default_device_name()
}

/// Claim a pairing code and remember the server.
///
/// The credential returned by the server goes straight into Windows
/// Credential Manager inside this function and is dropped on the way out.
#[tauri::command]
pub async fn pair_server(
    app: AppHandle,
    state: State<'_, AppState>,
    address: String,
    code: String,
    device_name: String,
) -> Result<PairResult, AppError> {
    // Re-verify rather than trusting an address the UI is holding: the server
    // behind it is what the credential will be bound to.
    let verified = discovery::verify_address(&address).await?;
    if !verified.pairing_available {
        return Err(from_code("INSTANCE_NOT_READY"));
    }

    let origin = candidate_origins(&verified.base_url)?
        .into_iter()
        .next()
        .ok_or_else(|| from_code("ADDRESS_INVALID"))?;

    let outcome = pairing::pair(&origin, &code, &device_name).await?;
    let device_name = outcome.device.name.clone();

    // The one and only place the secret is touched above the transport layer.
    state
        .credentials
        .save(&verified.server_id, &outcome.into_credential())?;

    ServerStore::new(&app)?.upsert(SavedServer {
        server_id: verified.server_id.clone(),
        name: verified.name.clone(),
        base_url: verified.base_url.clone(),
        protocol_version: verified.protocol_version,
        last_connected_at: None,
        revoked: false,
    })?;

    Ok(PairResult {
        paired: true,
        server_id: verified.server_id,
        device_name,
    })
}

/// Connect to a saved, paired server and open the real Arciin interface.
#[tauri::command]
pub async fn connect_server(
    app: AppHandle,
    state: State<'_, AppState>,
    server_id: String,
) -> Result<(), AppError> {
    connection::connect(&app, &server_id, state.credentials.as_ref()).await
}

/// Forget a server on this computer.
///
/// Removes the saved metadata, deletes the OS credential for this server only,
/// and clears the WebView session for that origin. This does **not** revoke the
/// device server-side; that remains an owner/admin action in
/// Settings -> Devices.
#[tauri::command]
pub async fn forget_server(
    app: AppHandle,
    state: State<'_, AppState>,
    server_id: String,
) -> Result<(), AppError> {
    let store = ServerStore::new(&app)?;
    let saved = store.get(&server_id);

    state.credentials.delete(&server_id)?;
    store.remove(&server_id)?;

    if let Some(saved) = saved {
        if let Some(window) = app.get_webview_window(connection::ONBOARDING_WINDOW) {
            // Best effort: a stale cookie for a forgotten server is harmless,
            // and failing here must not block the forget itself.
            if let Err(err) = connection::webview::clear_origin_session(&window, &saved.base_url) {
                tracing::warn!(code = %err.code, "webview session could not be cleared");
            }
        }
    }

    if app.get_window(ARCIIN_WINDOW).is_some() {
        connection::return_to_onboarding(&app);
    }

    tracing::info!(server_id, "server forgotten on this computer");
    Ok(())
}

/// Close the remote Arciin window and show onboarding again.
#[tauri::command]
pub fn show_onboarding(app: AppHandle) {
    connection::return_to_onboarding(&app);
}

// --- Computer backup ----------------------------------------------------
//
// Note what none of these take: a server id, an origin, or a credential. The
// server being acted on comes from the verified connection recorded natively,
// and secrets never cross this boundary in either direction.

/// The connected server, or an error if nothing is connected yet.
fn active(state: &State<'_, AppState>) -> Result<crate::ActiveConnection, AppError> {
    state
        .connection
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| from_code("SERVER_NOT_SAVED"))
}

/// Can backup be offered for the connected server, and is it already on?
#[tauri::command]
pub fn backup_availability(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<crate::backup::manager::BackupAvailability, AppError> {
    let connection = active(&state)?;

    // "Signed in" means a user session exists in the WebView. The session
    // itself is not read here, only its presence.
    let signed_in = app
        .get_webview_window(connection::ONBOARDING_WINDOW)
        .and_then(|window| {
            connection::webview::borrow_user_session(&window, &connection.origin).ok()
        })
        .map(|session| session.is_some())
        .unwrap_or(false);

    state.backup.availability(
        &app,
        &connection.server_id,
        connection.backup_supported,
        signed_in,
    )
}

/// Known folders on this machine that can be protected.
#[tauri::command]
pub fn backup_folders(
    state: State<'_, AppState>,
) -> Vec<crate::backup::manager::ProtectableFolder> {
    state.backup.protectable_folders()
}

/// Measure one folder's size and file count.
#[tauri::command]
pub async fn backup_measure_folder(
    state: State<'_, AppState>,
    id: String,
) -> Result<crate::backup::scan::ScanSummary, AppError> {
    state.backup.measure_folder(&id).await
}

/// Open the Windows folder picker so a folder outside the known folders can be
/// protected.
///
/// Takes no arguments: there is no way to ask for a *particular* folder, only
/// for the dialog, which a person then operates. Returns the chosen folder's
/// handle and name — never its path — or `None` if they cancelled.
///
/// Only the onboarding window can reach this. The webview showing the server's
/// page is in no capability and has no IPC at all, so the remote origin can
/// neither call this nor learn its result.
#[tauri::command]
pub async fn backup_pick_folder(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Option<crate::backup::manager::ProtectableFolder>, AppError> {
    // Parented to the window the person is looking at, so the dialog opens in
    // front of it and moves with it rather than appearing behind.
    let parent = app
        .get_webview_window(connection::ONBOARDING_WINDOW)
        .and_then(|window| window.hwnd().ok())
        .map(|hwnd| hwnd.0 as isize);

    state.backup.pick_custom_folder(parent)
}

/// Abandon an in-flight measurement.
#[tauri::command]
pub fn backup_cancel_measuring(state: State<'_, AppState>) {
    state.backup.cancel_measuring();
}

/// Turn backup on for the chosen folders.
///
/// Returns `enabled: true` and progress counters. It does **not** return the
/// `arcsync_` credential, and there is no command that does.
#[tauri::command]
pub async fn backup_enable(
    app: AppHandle,
    state: State<'_, AppState>,
    selections: Vec<String>,
) -> Result<crate::backup::manager::BackupState, AppError> {
    let connection = active(&state)?;
    if !connection.backup_supported {
        return Err(from_code("BACKUP_NOT_SUPPORTED"));
    }

    // Borrowed for exactly this call, then dropped. Authorizing backup needs a
    // signed-in person, and that session only exists in the WebView.
    let window = app
        .get_webview_window(connection::ONBOARDING_WINDOW)
        .ok_or_else(|| from_code("WEBVIEW_BRIDGE_UNAVAILABLE"))?;
    let session = connection::webview::borrow_user_session(&window, &connection.origin)?
        .ok_or_else(|| from_code("BACKUP_UNAUTHORIZED"))?;
    let cookie_header = session.cookie_header();

    state
        .backup
        .enable(
            &app,
            state.credentials.as_ref(),
            &connection.server_id,
            &connection.origin,
            &connection.device_id,
            &cookie_header,
            &selections,
        )
        .await
}

/// How much room the Arciin server has.
///
/// Borrowed session, one read, nothing stored. Answers "will it fit?", which
/// counting local files cannot.
#[tauri::command]
pub async fn backup_server_storage(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<crate::backup::client::ServerStorage, AppError> {
    let connection = active(&state)?;
    let window = app
        .get_webview_window(connection::ONBOARDING_WINDOW)
        .ok_or_else(|| from_code("WEBVIEW_BRIDGE_UNAVAILABLE"))?;
    let session = connection::webview::borrow_user_session(&window, &connection.origin)?
        .ok_or_else(|| from_code("BACKUP_UNAUTHORIZED"))?;

    crate::backup::client::server_storage(&connection.origin, &session.cookie_header()).await
}

/// Current backup state for the connected server.
#[tauri::command]
pub fn backup_state(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<crate::backup::manager::BackupState, AppError> {
    let connection = active(&state)?;
    state.backup.state(&app, &connection.server_id)
}

/// Stop starting new transfers. The queue and all state are kept.
#[tauri::command]
pub fn backup_pause(app: AppHandle, state: State<'_, AppState>) -> Result<(), AppError> {
    let connection = active(&state)?;
    state.backup.pause(&app, &connection.server_id)
}

#[tauri::command]
pub fn backup_resume(app: AppHandle, state: State<'_, AppState>) -> Result<(), AppError> {
    let connection = active(&state)?;
    state.backup.resume(&app, &connection.server_id)
}

/// Turn backup off on this computer.
///
/// Drops the sync credential and the local queue. Local files are untouched
/// and the computer stays paired.
#[tauri::command]
pub fn backup_forget(app: AppHandle, state: State<'_, AppState>) -> Result<(), AppError> {
    let connection = active(&state)?;
    state
        .backup
        .forget(&app, state.credentials.as_ref(), &connection.server_id)
}

/// Close the native backup surface and return to the running Arciin window.
///
/// Native rather than a route change: the old "back" only changed a React
/// route, leaving the onboarding window on top of a perfectly healthy Arciin
/// window and looking like a restart.
#[tauri::command]
pub fn close_backup_ui(app: AppHandle) {
    connection::close_backup_ui(&app);
}

/// Stop protecting one folder.
///
/// Keeps the profile, the Device pairing, every other folder, and the copy
/// already on the server. Nothing on this PC is touched.
#[tauri::command]
pub fn backup_remove_root(
    app: AppHandle,
    state: State<'_, AppState>,
    root_id: String,
) -> Result<crate::backup::manager::BackupState, AppError> {
    let connection = active(&state)?;
    state
        .backup
        .remove_root(&app, &connection.server_id, &root_id)
}

/// Open a protected folder in File Explorer.
///
/// The path is resolved natively from the root id; the renderer never supplies
/// one, so there is nothing here that could be pointed at an arbitrary
/// location.
#[tauri::command]
pub fn backup_open_root(
    app: AppHandle,
    state: State<'_, AppState>,
    root_id: String,
) -> Result<(), AppError> {
    let connection = active(&state)?;
    let path = state
        .backup
        .root_path(&app, &connection.server_id, &root_id)?;
    tauri_plugin_opener::open_path(path, None::<&str>)
        .map_err(|_| from_code("BACKUP_FOLDER_UNAVAILABLE"))
}

/// Size the window for the Backup Center.
#[tauri::command]
pub fn size_for_backup_center(app: AppHandle) {
    connection::size_for_backup_center(&app);
}

/// Show the onboarding window.
///
/// The window starts hidden and stays hidden through an automatic reconnect,
/// so a paired computer goes from launch straight to Arciin without the setup
/// shell flashing past. The frontend calls this as soon as it has something
/// the user actually needs to see.
#[tauri::command]
pub fn reveal_onboarding(app: AppHandle) {
    connection::reveal_onboarding(&app);
}
