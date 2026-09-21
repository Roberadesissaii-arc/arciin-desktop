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
