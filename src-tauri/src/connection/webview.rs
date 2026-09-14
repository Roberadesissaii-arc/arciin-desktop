//! The device-to-WebView session bridge.
//!
//! # What problem this solves
//!
//! The permanent device credential lives in Windows Credential Manager and is
//! used only by Rust. The real Arciin interface, however, runs inside WebView2
//! against the remote server origin, and the server recognises a trusted
//! device by an **HttpOnly cookie** (`arciin_trusted_device`, protocol
//! section 6) - not by a header it can ask Rust for.
//!
//! So the two halves have to be joined. Rust performs the bootstrap, the
//! server answers with its own `Set-Cookie`, and that cookie has to end up in
//! the WebView's cookie jar rather than in the Rust HTTP client's.
//!
//! # How
//!
//! `ICoreWebView2_2::CookieManager` is WebView2's first-class API for exactly
//! this. The cookie written here is:
//!
//! - the **server's own token**, verbatim from its `Set-Cookie` response
//!   header - nothing is minted client-side;
//! - **HttpOnly**, so scripts running in the Arciin page cannot read it,
//!   exactly as if the browser had received the response itself;
//! - **short-lived**, expiring at the `expiresAt` the server returned (12h);
//! - **scoped to one host**, with `Secure` set whenever the origin is HTTPS.
//!
//! # What is deliberately never done
//!
//! - the permanent credential is never given to the WebView, in any form;
//! - no secret is ever put in a URL, a query string, or a fragment;
//! - no secret is ever injected into JavaScript or onto `window`;
//! - `localStorage` is not used for any of this;
//! - no long-lived JavaScript-readable token is created.
//!
//! This is a transport detail, not a new protocol: the bytes that reach the
//! cookie jar are the bytes the server asked a client to store.

use std::sync::mpsc;
use std::time::Duration;

use url::Url;

use crate::error::{from_code, AppError};
use crate::pairing::DeviceSession;

/// How long to wait for the closure dispatched onto the UI thread.
const BRIDGE_TIMEOUT: Duration = Duration::from_secs(10);

#[cfg(windows)]
mod imp {
    use super::*;

    use webview2_com::Microsoft::Web::WebView2::Win32::{
        ICoreWebView2CookieManager, ICoreWebView2_2, COREWEBVIEW2_COOKIE_SAME_SITE_KIND_LAX,
    };
    use windows::core::{Interface, HSTRING, PCWSTR};

    /// Reach the cookie manager from a controller handed to us by Tauri.
    fn cookie_manager(
        webview: &tauri::webview::PlatformWebview,
    ) -> Result<ICoreWebView2CookieManager, AppError> {
        // SAFETY: the controller comes from Tauri's own WebView2 instance and
        // this runs on the UI thread that owns it.
        unsafe {
            let core = webview.controller().CoreWebView2().map_err(|err| {
                tracing::error!(error = %err, "no CoreWebView2 on the controller");
                from_code("WEBVIEW_BRIDGE_UNAVAILABLE")
            })?;
            let core2: ICoreWebView2_2 = core.cast().map_err(|err| {
                tracing::error!(error = %err, "WebView2 runtime is too old for CookieManager");
                from_code("WEBVIEW_BRIDGE_UNAVAILABLE")
            })?;
            core2.CookieManager().map_err(|err| {
                tracing::error!(error = %err, "cookie manager unavailable");
                from_code("WEBVIEW_BRIDGE_UNAVAILABLE")
            })
        }
    }

    /// Write the trusted-device cookie into the WebView's jar.
    pub fn write_cookie(
        webview: &tauri::webview::PlatformWebview,
        host: &str,
        secure: bool,
        expires_unix: f64,
        name: &str,
        value: &str,
    ) -> Result<(), AppError> {
        let manager = cookie_manager(webview)?;

        let name = HSTRING::from(name);
        let value = HSTRING::from(value);
        let domain = HSTRING::from(host);
        let path = HSTRING::from("/");

        // SAFETY: every pointer below outlives the call, and the manager was
        // just obtained on this thread.
        unsafe {
            let cookie = manager
                .CreateCookie(
                    PCWSTR(name.as_ptr()),
                    PCWSTR(value.as_ptr()),
                    PCWSTR(domain.as_ptr()),
                    PCWSTR(path.as_ptr()),
                )
                .map_err(|err| {
                    tracing::error!(error = %err, "cookie could not be created");
                    from_code("WEBVIEW_BRIDGE_UNAVAILABLE")
                })?;

            // Mirror the attributes the server set on its own response, so the
            // cookie behaves identically to one a browser had received.
            cookie.SetIsHttpOnly(true).ok();
            cookie
                .SetSameSite(COREWEBVIEW2_COOKIE_SAME_SITE_KIND_LAX)
                .ok();
            cookie.SetIsSecure(secure).ok();
            cookie.SetExpires(expires_unix).ok();

            manager.AddOrUpdateCookie(&cookie).map_err(|err| {
                tracing::error!(error = %err, "cookie could not be stored in the webview");
                from_code("WEBVIEW_BRIDGE_UNAVAILABLE")
            })?;
        }

        Ok(())
    }

    /// Remove every cookie this origin has set in the WebView's jar.
    pub fn clear_cookies(
        webview: &tauri::webview::PlatformWebview,
        origin: &str,
    ) -> Result<(), AppError> {
        let manager = cookie_manager(webview)?;
        let uri = HSTRING::from(origin);
        // SAFETY: `uri` outlives the call.
        unsafe {
            manager
                .DeleteCookies(PCWSTR::null(), PCWSTR(uri.as_ptr()))
                .map_err(|err| {
                    tracing::error!(error = %err, "webview cookies could not be cleared");
                    from_code("WEBVIEW_BRIDGE_UNAVAILABLE")
                })?;
        }
        Ok(())
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;

    pub fn write_cookie(
        _webview: &tauri::webview::PlatformWebview,
        _host: &str,
        _secure: bool,
        _expires_unix: f64,
        _name: &str,
        _value: &str,
    ) -> Result<(), AppError> {
        Err(from_code("WEBVIEW_BRIDGE_UNAVAILABLE"))
    }

    pub fn clear_cookies(
        _webview: &tauri::webview::PlatformWebview,
        _origin: &str,
    ) -> Result<(), AppError> {
        Err(from_code("WEBVIEW_BRIDGE_UNAVAILABLE"))
    }
}

/// Parse the server's RFC 3339 `expiresAt` into a Unix timestamp.
///
/// If it cannot be read, fall back to twelve hours from now - the server's own
/// `DEVICE_SESSION_TTL_MS`. A wrong-but-short expiry is safe: the server
/// rejects a stale token regardless of what the cookie jar believes.
fn expires_unix(expires_at: &str) -> f64 {
    match chrono::DateTime::parse_from_rfc3339(expires_at) {
        Ok(parsed) => parsed.timestamp() as f64,
        Err(err) => {
            tracing::warn!(error = %err, "could not parse session expiry; using the default TTL");
            (chrono::Utc::now().timestamp() + 12 * 60 * 60) as f64
        }
    }
}

/// Run a closure against the platform WebView on the UI thread and wait for it.
pub(crate) fn on_webview<F, T>(window: &tauri::WebviewWindow, work: F) -> Result<T, AppError>
where
    F: FnOnce(&tauri::webview::PlatformWebview) -> Result<T, AppError> + Send + 'static,
    T: Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    window
        .with_webview(move |webview| {
            let _ = tx.send(work(&webview));
        })
        .map_err(|err| {
            tracing::error!(error = %err, "could not reach the platform webview");
            from_code("WEBVIEW_BRIDGE_UNAVAILABLE")
        })?;

    match rx.recv_timeout(BRIDGE_TIMEOUT) {
        Ok(result) => result,
        Err(err) => {
            tracing::error!(error = %err, "webview bridge did not respond in time");
            Err(from_code("WEBVIEW_BRIDGE_UNAVAILABLE"))
        }
    }
}

/// Install the trusted-device session into the WebView for `origin`.
///
/// After this returns, navigating that WebView to the Arciin origin carries
/// the trusted-device context the server issued - and the user still meets the
/// normal Arciin login, because device trust is not user authentication.
pub fn install_device_session(
    window: &tauri::WebviewWindow,
    origin: &Url,
    session: &DeviceSession,
) -> Result<(), AppError> {
    let Some(host) = origin.host_str().map(str::to_owned) else {
        return Err(from_code("ADDRESS_INVALID"));
    };
    let secure = origin.scheme() == "https";
    let expires = expires_unix(&session.expires_at);
    let name = session.cookie_name.clone();
    let value = session.cookie_value.clone();

    on_webview(window, move |webview| {
        imp::write_cookie(webview, &host, secure, expires, &name, &value)
    })?;

    // The value itself is never logged - only that the handover happened.
    tracing::info!(
        origin = %origin.origin().ascii_serialization(),
        "trusted-device session installed into the webview"
    );
    Ok(())
}

/// Forget every cookie this WebView holds for one server origin.
pub fn clear_origin_session(window: &tauri::WebviewWindow, origin: &str) -> Result<(), AppError> {
    let owned = origin.to_string();
    on_webview(window, move |webview| imp::clear_cookies(webview, &owned))?;
    tracing::info!(
        origin,
        "cleared webview session data for a forgotten server"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::expires_unix;

    #[test]
    fn parses_the_server_expiry() {
        let parsed = expires_unix("2026-09-12T23:00:00.000Z");
        assert_eq!(parsed as i64, 1789254000);
    }

    #[test]
    fn falls_back_to_a_short_ttl_when_unparseable() {
        let now = chrono::Utc::now().timestamp();
        let parsed = expires_unix("not-a-date") as i64;
        assert!(parsed > now, "fallback must be in the future");
        assert!(parsed <= now + 12 * 60 * 60 + 5, "fallback must stay short");
    }
}

// --- Reading the user's session out of the WebView ----------------------

/// Records which of Arciin's two apps served the last document. The server
/// routes `/_next/*` assets by this, *before* it looks at anything else.
const SURFACE_COOKIE: &str = "arciin_surface";

/// The explicit "show me this version" choice, which outranks the user agent.
const VIEW_COOKIE: &str = "arciin_view";

/// What this client always is.
const DESKTOP_SURFACE: &str = "desktop";

/// Arciin's session cookie (`SESSION_COOKIE_NAME`, default `arciin_session`).
const SESSION_COOKIE: &str = "arciin_session";

#[cfg(windows)]
fn read_cookie(
    webview: &tauri::webview::PlatformWebview,
    origin: &str,
    name: &str,
) -> Result<Option<String>, AppError> {
    use webview2_com::GetCookiesCompletedHandler;
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        ICoreWebView2CookieManager, ICoreWebView2_2,
    };
    use windows::core::{Interface, HSTRING, PCWSTR};

    // SAFETY: this runs on the UI thread that owns the controller, and every
    // string outlives the call it is passed to.
    unsafe {
        let core = webview
            .controller()
            .CoreWebView2()
            .map_err(|_| from_code("WEBVIEW_BRIDGE_UNAVAILABLE"))?;
        let core2: ICoreWebView2_2 = core
            .cast()
            .map_err(|_| from_code("WEBVIEW_BRIDGE_UNAVAILABLE"))?;
        let manager: ICoreWebView2CookieManager = core2
            .CookieManager()
            .map_err(|_| from_code("WEBVIEW_BRIDGE_UNAVAILABLE"))?;

        let uri = HSTRING::from(origin);
        let wanted = name.to_string();
        // The completion callback must be `'static`, so the result comes back
        // through a shared cell rather than a borrow.
        let found = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
        let sink = std::sync::Arc::clone(&found);

        GetCookiesCompletedHandler::wait_for_async_operation(
            Box::new(move |handler| {
                manager.GetCookies(PCWSTR(uri.as_ptr()), &handler)?;
                Ok(())
            }),
            Box::new(move |_hresult, list| {
                let Some(list) = list else { return Ok(()) };
                let mut count = 0u32;
                list.Count(&mut count)?;
                for index in 0..count {
                    let cookie = list.GetValueAtIndex(index)?;
                    let mut raw_name = windows::core::PWSTR::null();
                    cookie.Name(&mut raw_name)?;
                    let cookie_name = raw_name.to_string().unwrap_or_default();
                    windows::Win32::System::Com::CoTaskMemFree(Some(raw_name.0 as *const _));

                    if cookie_name != wanted {
                        continue;
                    }
                    let mut raw_value = windows::core::PWSTR::null();
                    cookie.Value(&mut raw_value)?;
                    let value = raw_value.to_string().unwrap_or_default();
                    windows::Win32::System::Com::CoTaskMemFree(Some(raw_value.0 as *const _));
                    if !value.is_empty() {
                        *sink.lock().unwrap() = Some(value);
                    }
                    break;
                }
                Ok(())
            }),
        )
        .map_err(|err| {
            tracing::error!(error = %err, "webview cookies could not be read");
            from_code("WEBVIEW_BRIDGE_UNAVAILABLE")
        })?;

        let value = found.lock().unwrap().take();
        Ok(value)
    }
}

#[cfg(not(windows))]
fn read_cookie(
    _webview: &tauri::webview::PlatformWebview,
    _origin: &str,
    _name: &str,
) -> Result<Option<String>, AppError> {
    Err(from_code("WEBVIEW_BRIDGE_UNAVAILABLE"))
}

/// A user session token, borrowed from the WebView for one authorized call.
///
/// Authorizing computer backup needs a signed-in *user*, not just a trusted
/// device — and that session only ever exists in the WebView's cookie jar,
/// because that is where the person actually signed in. Rust therefore borrows
/// it for exactly one request.
///
/// The rules this type exists to enforce:
///
/// - it is never written to disk, the sync database, or a log line;
/// - it is never handed to React;
/// - it is read at the moment it is needed and dropped immediately after.
///
/// It is the mirror of `install_device_session`: that writes a cookie the
/// server issued *into* the jar, this reads one the server issued *out* of it.
pub struct BorrowedSession(String);

impl BorrowedSession {
    /// The `Cookie` header value for a single request.
    pub fn cookie_header(&self) -> String {
        format!("{SESSION_COOKIE}={}", self.0)
    }

    /// Build one directly. Tests only: in the app a session is only ever
    /// obtained by reading it back out of the WebView.
    #[cfg(test)]
    pub fn from_raw(value: String) -> Self {
        Self(value)
    }
}

impl std::fmt::Debug for BorrowedSession {
    /// Redacted, so it cannot be leaked by a stray `{:?}`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BorrowedSession(<redacted>)")
    }
}

/// Borrow the signed-in user's session for the connected origin.
///
/// `Ok(None)` means nobody is signed in yet — an ordinary state, not a fault.
pub fn borrow_user_session(
    window: &tauri::WebviewWindow,
    origin: &Url,
) -> Result<Option<BorrowedSession>, AppError> {
    let origin_string = origin.origin().ascii_serialization();
    let value = on_webview(window, move |webview| {
        read_cookie(webview, &origin_string, SESSION_COOKIE)
    })?;

    // Only whether a session was found is logged, never the token.
    tracing::info!(
        origin = %origin.origin().ascii_serialization(),
        found = value.is_some(),
        "looked for a signed-in user session in the webview"
    );
    Ok(value.map(BorrowedSession))
}

/// Tell the server this window is the desktop surface.
///
/// Arciin serves two apps from one origin — the desktop UI and a mobile PWA —
/// and decides per request which answers. Assets are routed by the
/// `arciin_surface` cookie, which records what served the last document.
///
/// That cookie is sticky in a way that matters here. If it is ever left saying
/// `mobile`, every `/_next/*` request from the desktop app returns 404, and a
/// normal document request does **not** reset it. The page then renders with
/// no CSS at all — and because the "this screen is too small" notice is gated
/// purely on a `md:hidden` class, losing the stylesheet makes it appear. The
/// result looks like the app deciding a maximised desktop window is a phone.
///
/// This client is never the mobile surface, so it says so outright rather than
/// leaving the question to a user-agent guess and a cookie that cannot correct
/// itself: `arciin_view` states the preference, `arciin_surface` makes assets
/// resolve immediately instead of after a document round trip.
///
/// Neither cookie is a secret, and neither is HttpOnly server-side.
pub fn declare_desktop_surface(
    window: &tauri::WebviewWindow,
    origin: &Url,
) -> Result<(), AppError> {
    let Some(host) = origin.host_str().map(str::to_owned) else {
        return Err(from_code("ADDRESS_INVALID"));
    };
    let secure = origin.scheme() == "https";
    // A year out, matching what the server sets for its own view cookie.
    let expires = (chrono::Utc::now().timestamp() + 365 * 24 * 60 * 60) as f64;

    on_webview(window, move |webview| {
        for name in [VIEW_COOKIE, SURFACE_COOKIE] {
            imp::write_cookie(webview, &host, secure, expires, name, DESKTOP_SURFACE)?;
        }
        Ok(())
    })?;

    tracing::info!(
        origin = %origin.origin().ascii_serialization(),
        "declared this window as the desktop surface"
    );
    Ok(())
}

/// Drop the signed-in session cookie for one origin.
///
/// Used only to recover from a session that can never become device-bound.
/// The server stamps `Session.pairedDeviceId` at login and never revisits it,
/// so a session created before the trusted-device cookie existed is stuck
/// reporting this computer as somebody else's device forever. Clearing it
/// costs one sign-in and fixes it permanently; nothing else can.
pub fn clear_user_session(window: &tauri::WebviewWindow, origin: &Url) -> Result<(), AppError> {
    let Some(host) = origin.host_str().map(str::to_owned) else {
        return Err(from_code("ADDRESS_INVALID"));
    };
    let secure = origin.scheme() == "https";

    // Overwrite with an already-expired value rather than deleting by URI:
    // expiry is honoured for the exact host/path pair we wrote, which is the
    // one the server set.
    on_webview(window, move |webview| {
        imp::write_cookie(webview, &host, secure, 0.0, SESSION_COOKIE, "")
    })?;

    tracing::info!(
        origin = %origin.origin().ascii_serialization(),
        "cleared a web session that could not be device-bound"
    );
    Ok(())
}

// --- Refreshing the server's page after a native change ----------------------

/// Reload the Arciin page, but only if it is showing `path`.
///
/// # Why this exists
///
/// Turning backup on happens entirely in the native window. The server's My
/// Computers page has no idea it happened, so it kept showing "No protected
/// computers yet" with four zeroes until the person pressed F5 — right after
/// being told their backup had started.
///
/// # Why it is narrow
///
/// A blanket reload would be rude: it would throw away whatever the person was
/// doing on some unrelated page. So the current document's own URL is read
/// first and the reload happens only when they are looking at the page that is
/// now wrong. Anyone elsewhere is left alone and simply sees current data the
/// next time they visit.
///
/// `ICoreWebView2::Reload` is WebView2's own API — no script is injected into
/// the server's origin, which this client never does.
///
/// Returns whether a reload actually happened.
#[cfg(windows)]
pub fn reload_if_showing(webview: &tauri::Webview, path: &str) -> Result<bool, AppError> {
    let path = path.to_string();
    let (tx, rx) = mpsc::channel();

    webview
        .with_webview(move |platform| {
            // SAFETY: runs on the UI thread that owns the controller; the one
            // returned string is freed with the allocator WebView2 documents.
            let result = unsafe {
                platform
                    .controller()
                    .CoreWebView2()
                    .map_err(|_| from_code("WEBVIEW_BRIDGE_UNAVAILABLE"))
                    .and_then(|core| {
                        let mut raw = windows::core::PWSTR::null();
                        core.Source(&mut raw)
                            .map_err(|_| from_code("WEBVIEW_BRIDGE_UNAVAILABLE"))?;
                        let source = raw.to_string().unwrap_or_default();
                        windows::Win32::System::Com::CoTaskMemFree(Some(raw.0 as *const _));

                        let showing = Url::parse(&source)
                            .map(|url| url.path() == path)
                            .unwrap_or(false);
                        if !showing {
                            return Ok(false);
                        }

                        core.Reload()
                            .map_err(|_| from_code("WEBVIEW_BRIDGE_UNAVAILABLE"))?;
                        Ok(true)
                    })
            };
            let _ = tx.send(result);
        })
        .map_err(|err| {
            tracing::error!(error = %err, "could not reach the arciin webview to refresh it");
            from_code("WEBVIEW_BRIDGE_UNAVAILABLE")
        })?;

    match rx.recv_timeout(BRIDGE_TIMEOUT) {
        Ok(result) => result,
        Err(_) => Err(from_code("WEBVIEW_BRIDGE_UNAVAILABLE")),
    }
}

#[cfg(not(windows))]
pub fn reload_if_showing(_webview: &tauri::Webview, _path: &str) -> Result<bool, AppError> {
    Ok(false)
}
