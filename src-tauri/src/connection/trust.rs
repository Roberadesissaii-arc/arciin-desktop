//! Noticing when this computer stops being trusted, and tidying up after it.
//!
//! # Why a watchdog exists
//!
//! A person can disconnect this computer from inside the app itself — Settings
//! → Devices, in the server's own web UI, running in our WebView. The moment
//! they confirm, the server revokes the device, and every credential this app
//! holds becomes worthless.
//!
//! Nothing tells us. The WebView simply starts getting 401s and drifts to a
//! login page, which looks like a bug. So the native layer checks on its own.
//!
//! # How the check works
//!
//! Re-running the device bootstrap is the only side-effect-free way to ask
//! "is this device still trusted?" — and it is not really a side effect,
//! because issuing a new trusted-device session is exactly what we do at
//! connect time. The fresh cookie is installed back into the WebView, so the
//! probe doubles as keeping the 12-hour session from expiring underneath a
//! long-running window.
//!
//! # What a disconnect must and must not do
//!
//! Must: drop *this* server's device credential and put the user back on a
//! screen that explains itself.
//!
//! Must not: touch another server's credentials or state, or delete anything
//! the server holds. Disconnecting a computer is not a request to erase it.

use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager};

use crate::connection::{self, webview};
use crate::error::AppError;
use crate::servers::ServerStore;
use crate::AppState;

/// How often to confirm this computer is still trusted.
///
/// The common way a computer gets disconnected is the person doing it
/// themselves, in Settings -> Devices, in the window they are looking at. From
/// that moment the page is dead: the server deletes the device session *and*
/// the user session bound to it, so every request 401s. Anything slower than
/// this leaves them on a broken page wondering what they broke.
///
/// It is not free. The only route that validates a device credential is
/// `POST /api/devices/session`, which rotates the trusted-device session as a
/// side effect, so each check issues a new cookie that is immediately
/// reinstalled into the WebView. That is safe — per-request authentication
/// uses the *user* session, and `Session.pairedDeviceId` is bound at login —
/// but it is a write, so this is a balance rather than "as fast as possible".
const TRUST_CHECK_INTERVAL: Duration = Duration::from_secs(15);

/// Floor between checks, so a burst of page loads cannot hammer the server.
const TRUST_CHECK_THROTTLE: Duration = Duration::from_secs(5);

/// Emitted to the onboarding window when trust is lost.
pub const DEVICE_REVOKED_EVENT: &str = "arciin://device-revoked";

/// Error codes that mean "this computer is no longer trusted".
pub fn is_trust_lost(code: &str) -> bool {
    matches!(code, "DEVICE_REVOKED" | "DEVICE_INVALID")
}

/// Tear down everything tied to one server after it stops trusting us.
///
/// Scoped by `server_id` throughout: a second Arciin server's credentials and
/// state are untouched, and no local file is ever removed.
pub fn handle_device_revoked(app: &AppHandle, server_id: &str) {
    tracing::warn!(
        server_id,
        "this computer is no longer trusted; disconnecting"
    );

    let Some(state) = app.try_state::<AppState>() else {
        return;
    };

    // 1. Drop the device credential for this server only.
    if let Err(err) = state.credentials.delete(server_id) {
        tracing::warn!(code = %err.code, "device credential could not be cleared");
    }

    // 2. Keep the saved server, flagged, so the UI can explain what happened
    //    and offer to pair again rather than the server silently vanishing.
    if let Ok(store) = ServerStore::new(app) {
        if let Err(err) = store.update(server_id, |server| server.revoked = true) {
            tracing::info!(code = %err.code, "saved server could not be flagged as revoked");
        }
    }

    // 3. Forget the connection, so nothing native still thinks it is live.
    *state.connection.lock().unwrap() = None;

    // 4. Take down the Arciin window before it starts showing 401s, and put
    //    the onboarding window back with an explanation.
    connection::return_to_onboarding(app);

    if let Some(window) = app.get_webview_window(connection::ONBOARDING_WINDOW) {
        if let Err(err) = window.emit(DEVICE_REVOKED_EVENT, server_id) {
            tracing::warn!(error = %err, "could not tell the UI about the disconnect");
        }
    }

    tracing::info!(
        server_id,
        "disconnected cleanly; local files and other servers untouched"
    );
}

/// When the last trust check ran, as millis since the epoch.
///
/// Shared by the timer and the page-load trigger so the two cannot stack up.
static LAST_CHECK_MS: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

/// Claim the right to run a check now, or decline if one ran too recently.
fn claim_check_slot() -> bool {
    use std::sync::atomic::Ordering;

    let now = chrono::Utc::now().timestamp_millis();
    let last = LAST_CHECK_MS.load(Ordering::Relaxed);
    if now - last < TRUST_CHECK_THROTTLE.as_millis() as i64 {
        return false;
    }
    LAST_CHECK_MS.store(now, Ordering::Relaxed);
    true
}

/// What one trust check concluded.
enum CheckOutcome {
    /// Still trusted, or the answer was inconclusive.
    Continue,
    /// Trust is gone and the disconnect has been handled.
    Disconnected,
    /// This server is no longer the current one; stop watching it.
    StandDown,
}

/// Ask the server once whether this computer is still trusted.
async fn check_once(handle: &AppHandle, server_id: &str, origin: &url::Url) -> CheckOutcome {
    let Some(state) = handle.try_state::<AppState>() else {
        return CheckOutcome::StandDown;
    };

    // Stop quietly if the app has moved on to another server.
    let still_current = state
        .connection
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|c| c.server_id == server_id);
    if !still_current {
        return CheckOutcome::StandDown;
    }

    let Ok(Some(credential)) = state.credentials.load(server_id) else {
        // No credential left: either already cleaned up, or never stored.
        return CheckOutcome::StandDown;
    };

    match crate::pairing::bootstrap(origin, &credential).await {
        Ok(session) => {
            // Keep the WebView's session fresh with the new token.
            if let Some(window) = handle.get_webview_window(connection::ONBOARDING_WINDOW) {
                if let Err(err) = webview::install_device_session(&window, origin, &session) {
                    tracing::info!(code = %err.code, "could not refresh the webview session");
                }
            }
            CheckOutcome::Continue
        }
        Err(err) if is_trust_lost(&err.code) => {
            handle_device_revoked(handle, server_id);
            CheckOutcome::Disconnected
        }
        Err(err) => {
            // Offline, timeout, server restarting: not a revocation.
            tracing::info!(code = %err.code, "trust check could not reach the server");
            CheckOutcome::Continue
        }
    }
}

/// Check trust right now, rather than waiting for the next tick.
///
/// Called when the Arciin page loads, which is what a revocation usually
/// causes: the server deletes the user's session along with the device, so the
/// app bounces to sign-in. Catching that turns a wait into an instant answer.
pub fn check_soon(app: &AppHandle, server_id: String, origin: url::Url) {
    if !claim_check_slot() {
        return;
    }
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        check_once(&handle, &server_id, &origin).await;
    });
}

/// Start the background trust watchdog for the connected server.
///
/// Runs until the connection changes or trust is lost. Transient failures are
/// ignored — a server being briefly unreachable is not a revocation, and
/// treating it as one would unpair people every time their Wi-Fi dropped.
pub fn spawn_watchdog(app: &AppHandle, server_id: String, origin: url::Url) {
    let handle = app.clone();

    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(TRUST_CHECK_INTERVAL).await;
            if !claim_check_slot() {
                continue;
            }
            match check_once(&handle, &server_id, &origin).await {
                CheckOutcome::Continue => {}
                CheckOutcome::Disconnected => return,
                CheckOutcome::StandDown => {
                    tracing::info!(server_id = %server_id, "trust watchdog standing down");
                    return;
                }
            }
        }
    });
}

/// Report a failure seen elsewhere (a reconnect, say) and run the disconnect
/// flow if it means trust is gone.
///
/// Returns whether it was handled as a disconnect.
pub fn report_failure(app: &AppHandle, server_id: &str, error: &AppError) -> bool {
    if !is_trust_lost(&error.code) {
        return false;
    }
    handle_device_revoked(app, server_id);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revocation_codes_are_recognised() {
        // Both mean the same thing from two vantage points: the pairing
        // endpoint and the bootstrap.
        assert!(is_trust_lost("DEVICE_REVOKED"));
        assert!(is_trust_lost("DEVICE_INVALID"));
    }

    #[test]
    fn being_offline_is_not_a_revocation() {
        // The failure mode to avoid: unpairing someone because their Wi-Fi
        // dropped or the server restarted.
        for code in [
            "UNREACHABLE",
            "TIMEOUT",
            "RATE_LIMITED",
            "INTERNAL_ERROR",
            "TLS_ERROR",
        ] {
            assert!(!is_trust_lost(code), "{code} must not disconnect anyone");
        }
    }

    #[test]
    fn a_self_disconnect_is_noticed_quickly() {
        // Someone who disconnects their own computer is looking right at the
        // window. Leaving them on a dead 401 page for minutes reads as a bug.
        assert!(TRUST_CHECK_INTERVAL <= Duration::from_secs(20));
        // The check also keeps the WebView's cookie fresh, well inside the
        // server's 12-hour trusted-device TTL.
        assert!(TRUST_CHECK_INTERVAL < Duration::from_secs(12 * 60 * 60));
    }

    #[test]
    fn checks_cannot_stack_up() {
        // A burst of page loads must not become a burst of requests.
        assert!(claim_check_slot(), "the first check should be allowed");
        assert!(!claim_check_slot(), "an immediate second must be refused");
    }
}
