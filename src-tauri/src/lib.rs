//! Arciin Desktop - native shell for a private Arciin server.
//!
//! The application has two jobs and no others:
//!
//! 1. find, verify, pair with and authenticate to an Arciin server, holding
//!    the device credential natively and never handing it to a web context;
//! 2. open the server's own web interface once that is done.
//!
//! There is no copy of the Arciin product UI in this crate. Dashboard, Files,
//! Settings and the rest are served by the server, so a web release reaches
//! the desktop without a desktop release.

pub mod address;
pub mod chrome;
pub mod commands;
pub mod connection;
pub mod credentials;
pub mod discovery;
pub mod error;
pub mod http;
pub mod pairing;
pub mod protocol;
pub mod servers;

use std::sync::Arc;

use credentials::{CredentialStore, OsCredentialStore};

/// The server this app is currently connected to.
///
/// Recorded by the connection flow, which is the only code that has verified
/// it. Nothing takes a server's origin from the frontend: a renderer-supplied
/// one would let a page point native requests somewhere else.
#[derive(Debug, Clone)]
pub struct ActiveConnection {
    pub server_id: String,
    pub origin: url::Url,
    pub device_id: String,
}

/// Process-wide state.
///
/// Holds a handle to Windows Credential Manager (never a secret) and the
/// manager, and which server is connected.
pub struct AppState {
    pub credentials: Arc<dyn CredentialStore>,
    pub connection: std::sync::Mutex<Option<ActiveConnection>>,
    /// The webview showing the server's page.
    ///
    /// Held rather than looked up by label when needed: `get_webview` returns
    /// an `Option`, and a `None` from it fails silently at the call site. That
    /// already cost this project a day once, on the native action bridge.
    pub content_webview: std::sync::Mutex<Option<tauri::Webview<tauri::Wry>>>,
}

/// Development logging.
///
/// The policy this enforces is as important as the output: pairing codes,
/// device credentials, session cookies, passwords and `Authorization` headers
/// are never passed to a logging macro anywhere in this crate. What is logged
/// is the shape of a connection - host, server id, protocol version, state,
/// duration, error code.
fn init_logging() {
    use tracing_subscriber::{fmt, EnvFilter};

    let filter = EnvFilter::try_from_env("ARCIIN_LOG")
        .unwrap_or_else(|_| EnvFilter::new("arciin_desktop_lib=info,warn"));

    // A second initialisation in the same process is not an error worth
    // crashing over.
    let _ = fmt().with_env_filter(filter).with_target(true).try_init();
}

/// Create the onboarding window.
///
/// It is built here rather than declared in `tauri.conf.json` so it can start
/// hidden and be revealed the moment its content has loaded. Two things follow
/// from doing the reveal natively:
///
/// - Windows never shows an empty white frame before React paints;
/// - the window cannot be left invisible by a frontend that failed to start,
///   which a JavaScript-driven reveal allows.
fn build_onboarding_window(app: &tauri::AppHandle) -> tauri::Result<()> {
    tauri::WebviewWindowBuilder::new(
        app,
        connection::ONBOARDING_WINDOW,
        tauri::WebviewUrl::default(),
    )
    .title("Arciin Desktop")
    .inner_size(1400.0, 900.0)
    .min_inner_size(900.0, 600.0)
    .center()
    .resizable(true)
    // The Arciin canvas colour, so the frame matches the app before first paint.
    .background_color(tauri::window::Color(
        chrome::SHELL_R,
        chrome::SHELL_G,
        chrome::SHELL_B,
        255,
    ))
    .visible(false)
    .on_page_load(|window, payload| {
        if payload.event() != tauri::webview::PageLoadEvent::Finished {
            return;
        }
        // Paint the title bar now, while nothing is on screen, so the window
        // never appears with a default light caption first.
        //
        // Note what does *not* happen here: the window is not shown. On a
        // computer that is already paired, the app goes straight from launch
        // to the server's own UI, and flashing the setup shell on the way
        // makes a working reconnect look like a glitch. The frontend asks to
        // be shown only when it has something the user must act on.
        chrome::apply_dark_caption(&window);
    })
    .build()?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    init_logging();
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        protocol_version = protocol::DEVICE_PROTOCOL_VERSION,
        "arciin desktop starting"
    );

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(AppState {
            credentials: Arc::new(OsCredentialStore),
            connection: std::sync::Mutex::new(None),
            content_webview: std::sync::Mutex::new(None),
        })
        .setup(|app| {
            build_onboarding_window(app.handle())?;
            // Closing the settings surface returns to Arciin; closing Arciin
            // quits. Without this the hidden onboarding window kept the
            // process alive after its last visible window went away.
            connection::attach_onboarding_lifecycle(app.handle());
            connection::spawn_visibility_guard(app.handle());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::discover_servers,
            commands::verify_server,
            commands::saved_servers,
            commands::suggested_device_name,
            commands::pair_server,
            commands::connect_server,
            commands::forget_server,
            commands::show_onboarding,
            commands::reveal_onboarding,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Arciin Desktop");
}
