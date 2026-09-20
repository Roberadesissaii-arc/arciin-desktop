//! Turning a verified, paired server into an open Arciin window.
//!
//! The sequence, and why each step exists:
//!
//! ```txt
//! verify identity   the address may have been reassigned since we saved it
//!       |
//! load credential   from Windows Credential Manager, never from disk
//!       |
//! bootstrap         Rust proves device identity; server issues its cookie
//!       |
//! install cookie    handed to the WebView jar, HttpOnly, by the bridge
//!       |
//! open window       the real Arciin UI, which still shows its own login
//! ```
//!
//! Everything the server renders after this point is the server's own web
//! application. The desktop deliberately contains no copy of it.

pub mod bridge;
pub mod trust;
pub mod webview;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tauri::{AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, WebviewUrl};
use url::Url;

use crate::address::{is_safe_external, is_same_origin};
use crate::credentials::CredentialStore;
use crate::discovery::verify_expected_server;
use crate::error::{from_code, AppError};
use crate::pairing;
use crate::servers::ServerStore;

/// The window that hosts the remote Arciin interface.
pub const ARCIIN_WINDOW: &str = "arciin";

/// The webview inside it showing the server's own page.
pub const ARCIIN_CONTENT_WEBVIEW: &str = "arciin-content";

/// Our overlay webview holding the window controls.
///
/// A separate webview because the page underneath belongs to the server and
/// this client never injects script into a remote origin. The controls are
/// ours; the page is theirs; nothing crosses.
pub const ARCIIN_CHROME_WEBVIEW: &str = "arciin-chrome";

/// Height of the control strip.
///
/// The server's page is inset below it rather than sitting underneath it, so
/// nothing of Arciin's own UI is ever covered. The strip is painted the same
/// near-black as the sidebar, so the sidebar reads as continuous through it
/// and the only part that registers as a title bar is the stretch above the
/// light content pane.
///
/// Deliberately shorter than the 32px Windows draws for a real caption. Every
/// pixel here is a pixel taken off the sidebar, whose nav scrolls once its
/// items no longer fit — and the sidebar has more items now that Computers
/// exists. 26px still leaves the glyphs room to breathe.
const CHROME_HEIGHT: f64 = 26.0;

/// The window that hosts the local onboarding UI.
pub const ONBOARDING_WINDOW: &str = "main";

/// How long to wait for the Arciin window's first load before showing it
/// regardless, so a stalled page can never leave the app with no visible window.
const HANDOVER_FALLBACK: std::time::Duration = std::time::Duration::from_secs(15);

/// Breathing room between "the document finished loading" and showing it.
///
/// `PageLoadEvent::Finished` is WebView2's `NavigationCompleted`, which fires
/// once the document is parsed — *before* the stylesheets it references have
/// been applied. Revealing on that event puts a frame of unstyled HTML on
/// screen before the real page snaps in. The stylesheets are same-origin and
/// already fetched by this point, so a brief settle is all it takes for the
/// first painted frame to be the finished page.
const STYLE_SETTLE: std::time::Duration = std::time::Duration::from_millis(260);

/// Where a connection attempt got to. Reported to the UI so it can show the
/// right copy at each stage rather than one undifferentiated spinner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ConnectStage {
    Verifying,
    Authorizing,
    Securing,
    Opening,
}

/// Connect to an already-paired server and open the real Arciin UI.
///
/// Failures are mapped to stable codes; a revoked device additionally
/// disables the saved credential so the app stops retrying with something the
/// server will never accept again.
pub async fn connect(
    app: &AppHandle,
    server_id: &str,
    credentials: &dyn CredentialStore,
) -> Result<(), AppError> {
    let store = ServerStore::new(app)?;
    let Some(saved) = store.get(server_id) else {
        return Err(from_code("SERVER_NOT_SAVED"));
    };

    let started = std::time::Instant::now();
    tracing::info!(server_id, stage = ?ConnectStage::Verifying, "connecting");

    // The address may now belong to a different machine. Confirm identity
    // before the credential for this server is even read.
    let verified = verify_expected_server(&saved.base_url, server_id).await?;
    let origin = Url::parse(&verified.base_url).map_err(|_| from_code("ADDRESS_INVALID"))?;

    tracing::info!(server_id, stage = ?ConnectStage::Authorizing, "identity confirmed");

    let Some(credential) = credentials.load(server_id)? else {
        return Err(from_code("CREDENTIAL_MISSING"));
    };

    let session = match pairing::bootstrap(&origin, &credential).await {
        Ok(session) => session,
        Err(err) => {
            if err.code == "DEVICE_REVOKED" || err.code == "DEVICE_INVALID" {
                // Stop retrying with a credential the server has rejected, and
                // leave a record so the UI can explain rather than just fail.
                // Only this server is touched.
                credentials.delete(server_id)?;
                store.update(server_id, |server| server.revoked = true)?;
                tracing::warn!(server_id, code = %err.code, "device is no longer trusted; credential cleared");
            }
            return Err(err);
        }
    };

    let session_device_id = session.device.id.clone();
    tracing::info!(server_id, stage = ?ConnectStage::Securing, "device session issued");

    // The cookie jar is shared by every webview in this app's WebView2
    // profile, so installing through the onboarding window puts the session in
    // place *before* the Arciin window makes its first request. That ordering
    // matters: it avoids a first load that the server would see as untrusted.
    let onboarding = app
        .get_webview_window(ONBOARDING_WINDOW)
        .ok_or_else(|| from_code("WEBVIEW_BRIDGE_UNAVAILABLE"))?;
    webview::install_device_session(&onboarding, &origin, &session)?;

    // Arciin serves a desktop app and a mobile PWA from one origin and routes
    // assets by a cookie that cannot correct itself once wrong. Stating which
    // surface this is keeps the stylesheet from 404ing.
    if let Err(err) = webview::declare_desktop_surface(&onboarding, &origin) {
        tracing::warn!(code = %err.code, "could not declare the desktop surface");
    }

    // The server stamps `Session.pairedDeviceId` at login, from the
    // trusted-device cookie present at that instant, and never revisits it. A
    // session that predates this cookie is therefore stuck: Settings keeps
    // listing this computer under "Other devices" with a Revoke button however
    // correctly it is paired, and no amount of re-pairing fixes it.
    //
    // The cookie is in place by now, so a fresh sign-in will bind. Only clear
    // when the server positively says the session belongs to another device —
    // never on a failed or absent answer, which would log people out for a
    // network blip.
    if let Some(session) = webview::borrow_user_session(&onboarding, &origin)? {
        let binding =
            pairing::session_binding(&origin, &session.cookie_header(), &session_device_id).await;
        if binding == pairing::SessionBinding::Unbound {
            tracing::warn!(
                server_id,
                "the signed-in session is not bound to this device; clearing it so the next sign-in binds"
            );
            if let Err(err) = webview::clear_user_session(&onboarding, &origin) {
                tracing::warn!(code = %err.code, "stale session could not be cleared");
            }
        } else {
            tracing::info!(server_id, binding = ?binding, "web session device binding");
        }
    }

    tracing::info!(server_id, stage = ?ConnectStage::Opening, "opening arciin");

    open_arciin_window(app, &onboarding, &origin, server_id)?;

    store.update(server_id, |server| {
        server.name = verified.name.clone();
        server.protocol_version = verified.protocol_version;
        server.revoked = false;
        server.last_connected_at = Some(chrono::Utc::now().to_rfc3339());
    })?;

    // Recorded here, after verification, so backup commands never have to take
    // a server identity from the frontend.
    if let Some(state) = app.try_state::<crate::AppState>() {
        *state.connection.lock().unwrap() = Some(crate::ActiveConnection {
            server_id: server_id.to_string(),
            origin: origin.clone(),
            device_id: session.device.id.clone(),
            backup_supported: verified.backup_supported,
        });
    }

    // Pick up a backup that was already set up on this computer.
    //
    // Without this the engine only ever ran in the session that turned backup
    // on: every later launch left the queue sitting there uploading nothing,
    // while the UI cheerfully reported the counts from the database. Creates
    // nothing, so an interrupted first activation resumes rather than starting
    // a second profile.
    if verified.backup_supported {
        let resume_handle = app.clone();
        let resume_server = server_id.to_string();
        let resume_origin = origin.clone();
        tauri::async_runtime::spawn(async move {
            let Some(state) = resume_handle.try_state::<crate::AppState>() else {
                return;
            };
            let credentials = std::sync::Arc::clone(&state.credentials);
            if let Err(err) = state
                .backup
                .resume_existing(
                    &resume_handle,
                    credentials.as_ref(),
                    &resume_server,
                    &resume_origin,
                )
                .await
            {
                tracing::warn!(code = %err.code, "computer backup could not resume");
            }
        });
    }

    // Watch for this computer being disconnected from the server side — most
    // likely by the person themselves, in Settings -> Devices, inside the very
    // WebView we just opened.
    trust::spawn_watchdog(app, server_id.to_string(), origin.clone());

    tracing::info!(
        server_id,
        device_id = %session.device.id,
        backup_supported = verified.backup_supported,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "connected"
    );
    Ok(())
}

/// Where the onboarding window currently sits, in logical units.
///
/// The Arciin window is placed here so the handover looks like one window
/// changing its contents rather than the application restarting.
fn onboarding_geometry(onboarding: &tauri::WebviewWindow) -> Option<(f64, f64, f64, f64)> {
    let scale = onboarding.scale_factor().ok()?;
    let position = onboarding.outer_position().ok()?;
    let size = onboarding.inner_size().ok()?;
    Some((
        position.x as f64 / scale,
        position.y as f64 / scale,
        size.width as f64 / scale,
        size.height as f64 / scale,
    ))
}

/// Create (or re-show) the window that renders the remote Arciin application.
///
/// This window has no Tauri command access: its capability set is empty, so
/// the remote origin cannot invoke anything native even if a page on it tried.
///
/// The Arciin server sends `X-Frame-Options: DENY`, so its interface is loaded
/// as this window's own top-level navigation. It is never framed inside a
/// local page.
fn open_arciin_window(
    app: &AppHandle,
    onboarding: &tauri::WebviewWindow,
    origin: &Url,
    server_id: &str,
) -> Result<(), AppError> {
    if let Some(existing) = app.get_webview(ARCIIN_CONTENT_WEBVIEW) {
        existing.navigate(origin.clone()).map_err(|err| {
            tracing::error!(error = %err, "could not navigate the arciin window");
            AppError::internal("Arciin could not be opened.")
        })?;
        if let Some(window) = app.get_window(ARCIIN_WINDOW) {
            let _ = window.show();
            let _ = window.set_focus();
        }
        let _ = onboarding.hide();
        return Ok(());
    }

    let allowed = origin.clone();
    let handle = app.clone();
    let probe_handle = app.clone();
    let probe_origin = origin.clone();
    let probe_server = server_id.to_string();
    let maximized = onboarding.is_maximized().unwrap_or(false);
    let handed_over = Arc::new(AtomicBool::new(false));
    let handover_fallback = Arc::clone(&handed_over);

    // No native title bar. The sidebar the server draws then runs the full
    // height of the window instead of starting below a caption strip, and our
    // own controls sit over the top-right of the content area.
    let mut builder = tauri::window::WindowBuilder::new(app, ARCIIN_WINDOW)
        .title("Arciin")
        .min_inner_size(900.0, 600.0)
        .resizable(true)
        .maximized(maximized)
        .decorations(false)
        .background_color(tauri::window::Color(
            crate::chrome::SHELL_R,
            crate::chrome::SHELL_G,
            crate::chrome::SHELL_B,
            255,
        ))
        .visible(false);

    // Take over the onboarding window's exact footprint where we can read it.
    let (width, height) = match onboarding_geometry(onboarding) {
        Some((x, y, width, height)) => {
            builder = builder.position(x, y).inner_size(width, height);
            (width, height)
        }
        None => {
            builder = builder.inner_size(1400.0, 900.0).center();
            (1400.0, 900.0)
        }
    };

    let window = builder.build().map_err(|err| {
        tracing::error!(error = %err, "arciin window could not be created");
        AppError::internal("Arciin could not be opened.")
    })?;

    // The server's page, filling the window.
    let content = window
        .add_child(
            tauri::webview::WebviewBuilder::new(
                ARCIIN_CONTENT_WEBVIEW,
                WebviewUrl::External(origin.clone()),
            )
            // Let dropped files reach the page.
            //
            // Tauri installs an OS-level drag-and-drop handler by default, and
            // on Windows that handler swallows the drop before the webview
            // sees it: the HTML5 `dragover`/`drop` events never fire. Arciin's
            // own upload area is built on those events, so dragging a file
            // onto the window did nothing at all — no upload, and no sign that
            // anything had been dropped.
            //
            // Nothing here consumes Tauri's native drop events, so turning the
            // handler off loses no behaviour. It is also the narrower of the
            // two: the native handler hands the frontend real Windows paths,
            // where HTML5 hands it file contents and a name, which is all the
            // server's uploader needs.
            .disable_drag_drop_handler()
            .on_navigation(move |url| guard_navigation(&handle, &allowed, url))
            .on_page_load(move |webview, payload| {
                if payload.event() != tauri::webview::PageLoadEvent::Finished {
                    return;
                }

                // Revoking this computer deletes the user's session along with
                // the device, so Arciin bounces to sign-in. That reload is the
                // earliest signal available, and acting on it turns a wait
                // into an immediate answer. Throttled inside `check_soon`.
                trust::check_soon(&probe_handle, probe_server.clone(), probe_origin.clone());

                if handed_over.swap(true, Ordering::SeqCst) {
                    return;
                }
                let window = webview.window_ref().clone();
                std::thread::spawn(move || {
                    std::thread::sleep(STYLE_SETTLE);
                    let _ = window.show();
                    let _ = window.set_focus();
                    if let Some(onboarding) =
                        window.app_handle().get_webview_window(ONBOARDING_WINDOW)
                    {
                        let _ = onboarding.hide();
                    }
                });
            }),
            LogicalPosition::new(0.0, CHROME_HEIGHT),
            LogicalSize::new(width, (height - CHROME_HEIGHT).max(0.0)),
        )
        .map_err(|err| {
            tracing::error!(error = %err, "arciin content webview could not be created");
            AppError::internal("Arciin could not be opened.")
        })?;

    // Our controls, overlaid on the top-right corner.
    window
        .add_child(
            tauri::webview::WebviewBuilder::new(
                ARCIIN_CHROME_WEBVIEW,
                WebviewUrl::App("titlebar.html".into()),
            )
            // The same near-black as Arciin's sidebar, so the two read as one
            // surface. Transparency is not an option anyway: two sibling
            // WebView2 instances do not composite on Windows, and a
            // transparent overlay renders as nothing at all.
            .background_color(tauri::window::Color(
                crate::chrome::SHELL_R,
                crate::chrome::SHELL_G,
                crate::chrome::SHELL_B,
                255,
            )),
            LogicalPosition::new(0.0, 0.0),
            LogicalSize::new(width, CHROME_HEIGHT),
        )
        .map_err(|err| {
            tracing::error!(error = %err, "window controls could not be created");
            AppError::internal("Arciin could not be opened.")
        })?;

    // Kept so anything needing the server's page can reach it without a
    // lookup that might quietly return `None`.
    if let Some(state) = app.try_state::<crate::AppState>() {
        *state.content_webview.lock().unwrap() = Some(content);
    }

    attach_window_lifecycle(app);

    // Both children are positioned absolutely, so they have to be told when
    // the window changes size.
    let resize_target = window.clone();
    window.on_window_event(move |event| {
        if let tauri::WindowEvent::Resized(_) = event {
            layout_children(&resize_target);
        }
    });

    // Safety net. The window is hidden until its first load finishes, so if
    // that event never arrives the user would be left staring at a stale
    // "Connecting" screen with an invisible window behind it. Reveal it
    // anyway after a bounded wait; an Arciin error page is far better than
    // nothing at all.
    std::thread::spawn(move || {
        std::thread::sleep(HANDOVER_FALLBACK);
        if handover_fallback.swap(true, Ordering::SeqCst) {
            return;
        }
        tracing::warn!("arciin window did not finish loading in time; revealing it anyway");
        let _ = window.show();
        let _ = window.set_focus();
        if let Some(onboarding) = window.app_handle().get_webview_window(ONBOARDING_WINDOW) {
            let _ = onboarding.hide();
        }
    });

    Ok(())
}

/// Keep the two child webviews sized to the window.
///
/// The strip spans the top; the content webview takes everything below it, so
/// the server's page is never covered.
fn layout_children(window: &tauri::Window) {
    let Ok(size) = window.inner_size() else {
        return;
    };
    let scale = window.scale_factor().unwrap_or(1.0);
    let width = size.width as f64 / scale;
    let height = size.height as f64 / scale;

    if let Some(content) = window.get_webview(ARCIIN_CONTENT_WEBVIEW) {
        let _ = content.set_position(LogicalPosition::new(0.0, CHROME_HEIGHT));
        let _ = content.set_size(LogicalSize::new(width, (height - CHROME_HEIGHT).max(0.0)));
    }
    if let Some(chrome) = window.get_webview(ARCIIN_CHROME_WEBVIEW) {
        let _ = chrome.set_position(LogicalPosition::new(0.0, 0.0));
        let _ = chrome.set_size(LogicalSize::new(width, CHROME_HEIGHT));
    }
}

/// Decide whether the Arciin window may follow a navigation.
///
/// Only the connected server's origin renders in this window. Anything else -
/// an outbound link, a redirect to another host, and above all a
/// `javascript:` or `data:` URL - is refused here. A genuine external web
/// link is handed to the system browser instead of being dropped silently.
fn guard_navigation(app: &AppHandle, allowed: &Url, target: &Url) -> bool {
    let target_str = target.as_str();

    if is_same_origin(allowed, target_str) {
        return true;
    }

    // The one thing the server's page may ask the shell to do. Handled here
    // because the page's web message never reaches the native side under this
    // WebView2 runtime; see `bridge::SENTINEL_SCHEME`.
    //
    // Safe to act on without checking an origin: this webview only ever hosts
    // the trusted server, which is enforced by this very function.
    if let Some(action) = bridge::classify_navigation(target) {
        tracing::info!(?action, "native action accepted from the arciin page");
        match action {
            bridge::NativeAction::OpenComputerBackupSetup => open_backup_setup(app),
        }
        // Never navigates: the sentinel is a signal, not a destination.
        return false;
    }

    if is_safe_external(target_str) {
        tracing::info!(
            host = target.host_str().unwrap_or("unknown"),
            "opening an external link in the system browser"
        );
        if let Err(err) = tauri_plugin_opener::open_url(target_str, None::<&str>) {
            tracing::warn!(error = %err, "external link could not be opened");
        }
    } else {
        // `javascript:`, `data:`, `file:` and friends never leave this branch.
        tracing::warn!(
            scheme = target.scheme(),
            "blocked an unsupported navigation"
        );
    }

    false
}

/// Close the Arciin window and bring the onboarding UI back.
///
/// Used when a device is revoked or the user forgets a server: the remote
/// application must not stay on screen once its session is gone.
pub fn return_to_onboarding(app: &AppHandle) {
    if let Some(window) = app.get_window(ARCIIN_WINDOW) {
        let _ = window.close();
    }
    if let Some(window) = app.get_webview_window(ONBOARDING_WINDOW) {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// Show the onboarding window, if it is not already visible.
///
/// Called by the frontend the moment it has something the user must see. Kept
/// idempotent so every step transition can call it without thinking.
pub fn reveal_onboarding(app: &AppHandle) {
    let Some(window) = app.get_webview_window(ONBOARDING_WINDOW) else {
        return;
    };
    if window.is_visible().unwrap_or(false) {
        return;
    }
    crate::chrome::apply_dark_caption(&window);
    let _ = window.show();
    let _ = window.set_focus();
    tracing::info!("onboarding window revealed");
}

/// How long to wait before assuming the frontend is never going to ask.
///
/// Long enough for a normal auto-reconnect to have opened the Arciin window,
/// short enough that a genuine failure does not look like a hang.
const VISIBILITY_GUARD: std::time::Duration = std::time::Duration::from_secs(10);

/// Reveal the onboarding window if nothing else has appeared.
///
/// The onboarding window no longer shows itself on load, which is what keeps a
/// paired launch from flashing the setup shell. The cost of that is a failure
/// mode: a frontend that crashes before asking would leave the app running
/// with no window at all. This makes that impossible.
pub fn spawn_visibility_guard(app: &AppHandle) {
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(VISIBILITY_GUARD).await;

        // The Arciin window being up means the reconnect worked and the
        // onboarding window is meant to stay hidden.
        if handle.get_window(ARCIIN_WINDOW).is_some() {
            return;
        }
        let already_visible = handle
            .get_webview_window(ONBOARDING_WINDOW)
            .and_then(|window| window.is_visible().ok())
            .unwrap_or(false);
        if already_visible {
            return;
        }

        tracing::warn!("nothing asked to be shown in time; revealing onboarding anyway");
        reveal_onboarding(&handle);
    });
}

/// Bring the onboarding window forward on the folder-protection screen.
///
/// The Arciin window is left exactly as it is: not navigated, not reloaded.
/// The user came from My Computers and should find it unchanged behind them.
pub fn open_backup_setup(app: &AppHandle) {
    let Some(window) = app.get_webview_window(ONBOARDING_WINDOW) else {
        return;
    };

    reveal_onboarding(app);
    if let Err(err) = window.emit(trust::OPEN_BACKUP_SETUP_EVENT, ()) {
        tracing::warn!(error = %err, "could not open the backup setup screen");
        return;
    }
    tracing::info!("opened native backup setup from the arciin page");
}

/// Hide the native backup surface and hand focus back to Arciin.
///
/// # The bug this exists to fix
///
/// Backing out of the backup screens used to do nothing but change a React
/// route. The onboarding window stayed on top showing its own status copy —
/// "Arciin is open." / "Opening…" — while the real Arciin window sat alive
/// behind it. It read as though the app had restarted and was reconnecting,
/// when in fact nothing had happened at all.
///
/// So closing is a native operation, not a route change. The Arciin window is
/// never touched beyond being shown and focused: same window, same webview,
/// same session, same page. Nothing is rediscovered, re-paired or rebuilt.
pub fn close_backup_ui(app: &AppHandle) {
    let Some(arciin) = app.get_window(ARCIIN_WINDOW) else {
        // No Arciin window to go back to — leave onboarding where it is
        // rather than hiding the only thing on screen.
        tracing::info!("no arciin window to return to; leaving onboarding visible");
        return;
    };

    // Show Arciin first, then hide onboarding. The other order leaves a frame
    // with no window of ours on screen, which flickers the desktop through.
    let _ = arciin.show();
    let _ = arciin.unminimize();
    let _ = arciin.set_focus();

    if let Some(onboarding) = app.get_webview_window(ONBOARDING_WINDOW) {
        let _ = onboarding.hide();
    }

    tracing::info!("returned to the existing arciin window");
}

/// Size the onboarding window for the backup surface it is about to show.
///
/// The first-run shell is a tall marketing layout; the Backup Center is a
/// settings surface and wants to be wider and shorter. Resizing the window we
/// already have avoids a second window with its own lifecycle.
pub fn size_for_backup_center(app: &AppHandle) {
    let Some(window) = app.get_webview_window(ONBOARDING_WINDOW) else {
        return;
    };
    let _ = window.set_size(tauri::Size::Logical(tauri::LogicalSize {
        width: 820.0,
        // Sized so the one scrolling list shows about four rows once the
        // fixed parts above and below it have taken their space. Still well
        // inside a 1080-tall screen with the taskbar and caption.
        height: 820.0,
    }));
    let _ = window.center();
}

/// Decide what closing a window means, once the app has more than one.
///
/// # Why this is explicit
///
/// There are two windows and they are not peers. The onboarding window hosts
/// setup *and* the native settings surfaces; the Arciin window hosts the
/// product. Which one is "the app" depends on whether a connection exists,
/// and inferring that from whichever window happens to still be alive is what
/// produced two separate bugs:
///
/// - **The app would not quit.** Closing Arciin left the *hidden* onboarding
///   window alive. Tauri exits when every window is destroyed, and a hidden
///   window is not destroyed, so the process lingered with nothing on screen.
///   That is why stopping it needed `taskkill /F` — which skips WebView2's
///   cookie flush and silently threw away the signed-in session on every
///   restart, "Remember me" included.
/// - **Back went to the wrong place.** Closing the native backup surface left
///   onboarding visible showing its own status copy, which read as a restart.
///
/// So closing is answered from application state rather than window state.
/// Set once the app is on its way out.
///
/// Needed because the two close handlers would otherwise veto each other:
/// closing Arciin asks the onboarding window to close, and onboarding's own
/// handler — whose job is to turn a close into "go back" — sees the Arciin
/// window still alive mid-close and prevents it. The result was an app with no
/// windows that never exited.
static SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);

pub fn attach_window_lifecycle(app: &AppHandle) {
    // Closing the Arciin window means closing the app. Take the onboarding
    // window down with it so every window is destroyed and Tauri can run its
    // normal exit — which is what lets WebView2 flush its cookie store.
    if let Some(arciin) = app.get_window(ARCIIN_WINDOW) {
        let handle = app.clone();
        arciin.on_window_event(move |event| {
            if !matches!(event, tauri::WindowEvent::CloseRequested { .. }) {
                return;
            }
            tracing::info!("arciin window closed; shutting down");
            SHUTTING_DOWN.store(true, Ordering::SeqCst);
            if let Some(onboarding) = handle.get_webview_window(ONBOARDING_WINDOW) {
                let _ = onboarding.close();
            }
            // Belt and braces: if anything still holds a window open, exit
            // anyway rather than leaving a process with nothing on screen.
            // Runs after the close has been processed so WebView2 gets its
            // chance to flush the cookie store first.
            let exiting = handle.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(1500));
                exiting.exit(0);
            });
        });
    }
}

/// Closing the onboarding window while Arciin is running means "go back".
///
/// It is the native settings surface at that point, not the app. Quitting
/// because someone dismissed a settings window would be surprising, and
/// leaving it on screen was the original bug.
pub fn attach_onboarding_lifecycle(app: &AppHandle) {
    let Some(onboarding) = app.get_webview_window(ONBOARDING_WINDOW) else {
        return;
    };
    let handle = app.clone();
    onboarding.on_window_event(move |event| {
        let tauri::WindowEvent::CloseRequested { api, .. } = event else {
            return;
        };
        // Never veto during shutdown, or the app cannot exit.
        if SHUTTING_DOWN.load(Ordering::SeqCst) {
            return;
        }
        // Only when there is somewhere to go back to. With no Arciin window
        // this *is* the app, and closing it should quit.
        if handle.get_window(ARCIIN_WINDOW).is_some() {
            api.prevent_close();
            close_backup_ui(&handle);
        }
    });
}
