//! Claiming a pairing code, and proving device identity afterwards.
//!
//! Two distinct operations, deliberately in one module because they are the
//! two halves of "this computer is trusted":
//!
//! - `pair`      - one time, exchanges a 6-digit PIN for a permanent credential
//! - `bootstrap` - every launch, exchanges that credential for a session cookie
//!
//! Neither is user authentication. A paired device still shows the real Arciin
//! login before anyone can open a file.

use serde::{Deserialize, Serialize};
use url::Url;

use crate::error::{from_code, AppError};
use crate::http::{
    build_client, error_from_response, json_from_response, BOOTSTRAP_TIMEOUT, PAIRING_TIMEOUT,
};
use crate::protocol::{
    normalize_pairing_code, DEVICE_NAME_MAX_LENGTH, DEVICE_PROTOCOL_VERSION, DEVICE_SESSION_PATH,
    DEVICE_TYPE_DESKTOP, PAIR_PATH, PLATFORM_WINDOWS, TRUSTED_DEVICE_COOKIE,
};

/// The pairing claim body. Field names and value sets are fixed by the server
/// schema in `apps/api/src/modules/devices/routes.ts`.
///
/// Only what the protocol permits is sent. No MAC address, no hardware serial,
/// no machine fingerprint, no Windows product key.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PairRequest<'a> {
    code: &'a str,
    name: &'a str,
    platform: &'a str,
    device_type: &'a str,
    app_version: &'a str,
    protocol_version: u32,
}

#[derive(Debug, Deserialize)]
struct PairResponse {
    data: PairData,
}

#[derive(Debug, Deserialize)]
struct PairData {
    device: PairedDevice,
    /// Returned exactly once, by the server, and never again.
    credential: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairedDevice {
    pub id: String,
    pub name: String,
    pub platform: String,
    pub device_type: String,
    pub status: String,
}

/// The outcome of pairing, as seen by everything above this module.
///
/// The credential is a private field with no getter reachable from IPC: it
/// leaves this struct only by being handed to the credential store.
pub struct PairOutcome {
    pub device: PairedDevice,
    credential: String,
}

impl PairOutcome {
    /// Consume the outcome, yielding the secret exactly once at the point it
    /// is written to the OS store.
    pub fn into_credential(self) -> String {
        self.credential
    }
}

/// A default device name from the machine's hostname.
///
/// The hostname is already broadcast on the LAN by Windows itself, so this
/// discloses nothing new. It is truncated to the server's limit and falls back
/// to a generic name rather than failing.
pub fn default_device_name() -> String {
    let raw = gethostname::gethostname().to_string_lossy().to_string();
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "Windows PC".to_string();
    }
    trimmed.chars().take(DEVICE_NAME_MAX_LENGTH).collect()
}

/// Clamp a user-supplied device name to what the server will accept.
fn resolve_device_name(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return default_device_name();
    }
    trimmed.chars().take(DEVICE_NAME_MAX_LENGTH).collect()
}

/// Exchange a pairing code for a permanent device credential.
///
/// The code never appears in a URL, a query string or a log line - section 12
/// of the protocol is explicit about this, and so is the logging policy here.
pub async fn pair(origin: &Url, code: &str, device_name: &str) -> Result<PairOutcome, AppError> {
    let Some(normalized) = normalize_pairing_code(code) else {
        return Err(from_code("PAIRING_CODE_MALFORMED"));
    };

    let name = resolve_device_name(device_name);

    let url = origin
        .join(PAIR_PATH)
        .map_err(|_| from_code("ADDRESS_INVALID"))?;

    let body = PairRequest {
        code: &normalized,
        name: &name,
        platform: PLATFORM_WINDOWS,
        device_type: DEVICE_TYPE_DESKTOP,
        app_version: env!("CARGO_PKG_VERSION"),
        protocol_version: DEVICE_PROTOCOL_VERSION,
    };

    tracing::info!(
        origin = %origin.origin().ascii_serialization(),
        device_name = %name,
        "claiming pairing code"
    );

    let client = build_client(PAIRING_TIMEOUT)?;
    let response = client.post(url).json(&body).send().await?;

    if !response.status().is_success() {
        let err = error_from_response(response).await;
        tracing::warn!(code = %err.code, "pairing rejected");
        return Err(err);
    }

    let parsed: PairResponse = json_from_response(response).await?;
    tracing::info!(
        device_id = %parsed.data.device.id,
        status = %parsed.data.device.status,
        "device paired"
    );

    Ok(PairOutcome {
        device: parsed.data.device,
        credential: parsed.data.credential,
    })
}

/// The short-lived, HttpOnly session the server issues to a trusted device.
///
/// `cookie_value` is the raw token from the server's own `Set-Cookie` header.
/// It is *not* the permanent credential: it expires (12h server-side) and is
/// the exact value a browser would have stored had the browser made this call.
pub struct DeviceSession {
    pub cookie_name: String,
    pub cookie_value: String,
    /// RFC 3339, from the response body.
    pub expires_at: String,
    pub device: PairedDevice,
}

#[derive(Debug, Deserialize)]
struct SessionResponse {
    data: SessionData,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionData {
    device: PairedDevice,
    expires_at: String,
}

/// Pull one cookie's value out of a `Set-Cookie` header.
fn cookie_value_from_header(header: &str, name: &str) -> Option<String> {
    let first = header.split(';').next()?.trim();
    let (key, value) = first.split_once('=')?;
    if key.trim() != name {
        return None;
    }
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    Some(value.to_string())
}

/// Device bootstrap: prove this is a previously paired device.
///
/// The permanent credential goes out in an `Authorization: Device ...` header,
/// never a query string, and the server answers with a `Set-Cookie` for
/// `arciin_trusted_device`. That cookie, not the credential, is what the
/// WebView will later carry.
pub async fn bootstrap(origin: &Url, credential: &str) -> Result<DeviceSession, AppError> {
    let url = origin
        .join(DEVICE_SESSION_PATH)
        .map_err(|_| from_code("ADDRESS_INVALID"))?;

    tracing::info!(
        origin = %origin.origin().ascii_serialization(),
        "bootstrapping trusted device session"
    );

    let client = build_client(BOOTSTRAP_TIMEOUT)?;
    let response = client
        .post(url)
        .header("Authorization", format!("Device {credential}"))
        .send()
        .await?;

    if !response.status().is_success() {
        let err = error_from_response(response).await;
        tracing::warn!(code = %err.code, "device bootstrap rejected");
        return Err(err);
    }

    // Read the header before the body consumes the response.
    let cookie_value = response
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find_map(|header| cookie_value_from_header(header, TRUSTED_DEVICE_COOKIE));

    let Some(cookie_value) = cookie_value else {
        tracing::error!("bootstrap succeeded but set no trusted-device cookie");
        return Err(from_code("WEBVIEW_BRIDGE_UNAVAILABLE"));
    };

    let parsed: SessionResponse = json_from_response(response).await?;

    tracing::info!(
        device_id = %parsed.data.device.id,
        expires_at = %parsed.data.expires_at,
        "trusted device session issued"
    );

    Ok(DeviceSession {
        cookie_name: TRUSTED_DEVICE_COOKIE.to_string(),
        cookie_value,
        expires_at: parsed.data.expires_at,
        device: parsed.data.device,
    })
}

#[cfg(test)]
mod tests {
    use super::{cookie_value_from_header, resolve_device_name};
    use crate::protocol::DEVICE_NAME_MAX_LENGTH;

    #[test]
    fn reads_the_named_cookie() {
        let header = "arciin_trusted_device=abc123; Path=/; HttpOnly; SameSite=Lax";
        assert_eq!(
            cookie_value_from_header(header, "arciin_trusted_device"),
            Some("abc123".to_string())
        );
    }

    #[test]
    fn ignores_other_cookies() {
        let header = "arciin_session=xyz; Path=/; HttpOnly";
        assert_eq!(
            cookie_value_from_header(header, "arciin_trusted_device"),
            None
        );
    }

    #[test]
    fn rejects_an_empty_value() {
        let header = "arciin_trusted_device=; Path=/";
        assert_eq!(
            cookie_value_from_header(header, "arciin_trusted_device"),
            None
        );
    }

    #[test]
    fn blank_device_name_falls_back_to_the_hostname() {
        assert!(!resolve_device_name("   ").is_empty());
    }

    #[test]
    fn device_name_is_clamped_to_the_server_limit() {
        let long = "a".repeat(DEVICE_NAME_MAX_LENGTH + 40);
        assert_eq!(
            resolve_device_name(&long).chars().count(),
            DEVICE_NAME_MAX_LENGTH
        );
    }
}

/// Whether the signed-in web session is bound to this paired device.
///
/// The server sets `Session.pairedDeviceId` **once, at login**, from the
/// trusted-device cookie present at that moment. It is never re-evaluated. So a
/// session created before the cookie existed stays unbound for its whole life,
/// and `GET /api/settings/devices` keeps reporting this computer under "Other
/// devices" with a Revoke button, no matter how correctly it is paired.
///
/// Revocation does not clear it either: the server deletes sessions *bound* to
/// a revoked device, so an unbound one survives every re-pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionBinding {
    /// The session is bound to the device we are paired as.
    Bound,
    /// A session exists but is bound to nothing, or to another device.
    Unbound,
    /// Could not be determined — no session, or the request failed.
    Unknown,
}

#[derive(Debug, Deserialize)]
struct MeEnvelope {
    data: MeData,
}

#[derive(Debug, Deserialize)]
struct MeData {
    #[serde(default)]
    session: Option<MeSession>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MeSession {
    /// Server-derived from the `Session` row. Null in a normal browser, and
    /// null for a desktop session created before the trusted-device cookie
    /// existed.
    #[serde(default)]
    paired_device_id: Option<String>,
}

/// Ask the server which device this session is bound to.
///
/// Reads `GET /api/auth/me` -> `session.pairedDeviceId`, which every
/// authenticated role can call. `GET /api/settings/devices` would answer the
/// same question but is OWNER/ADMIN only, so a MEMBER signing in on the
/// desktop would get a 403 and look permanently unbound.
///
/// Identity is entirely server-derived: this never sends a hostname, IP, user
/// agent or device name that could influence the answer.
pub async fn session_binding(
    origin: &Url,
    session_cookie_header: &str,
    our_device_id: &str,
) -> SessionBinding {
    let Ok(url) = origin.join("/api/auth/me") else {
        return SessionBinding::Unknown;
    };
    let Ok(client) = build_client(BOOTSTRAP_TIMEOUT) else {
        return SessionBinding::Unknown;
    };

    let response = match client
        .get(url)
        .header("Cookie", session_cookie_header)
        .send()
        .await
    {
        Ok(response) => response,
        Err(_) => return SessionBinding::Unknown,
    };

    // 401/403 simply means nobody is signed in yet; that is not "unbound".
    if !response.status().is_success() {
        return SessionBinding::Unknown;
    }

    let Ok(parsed) = json_from_response::<MeEnvelope>(response).await else {
        return SessionBinding::Unknown;
    };

    // No session at all means nobody is signed in — not a stale binding.
    let Some(session) = parsed.data.session else {
        return SessionBinding::Unknown;
    };

    match session.paired_device_id.as_deref() {
        Some(bound) if bound == our_device_id => SessionBinding::Bound,
        // Both `null` (never bound) and a different device id mean the same
        // thing for us: this session can never become ours, and only a fresh
        // sign-in can fix it.
        _ => SessionBinding::Unbound,
    }
}

#[cfg(test)]
mod binding_tests {
    use super::*;

    const OURS: &str = "cmu0n11fc000dtobwex5teqfg";
    const THEIRS: &str = "cmtzc4ptu0009to88xj789y6q";

    /// Mirrors what `session_binding` concludes from a parsed response, so the
    /// decision is testable without a live server.
    fn decide(paired_device_id: Option<&str>, ours: &str) -> SessionBinding {
        match paired_device_id {
            Some(bound) if bound == ours => SessionBinding::Bound,
            _ => SessionBinding::Unbound,
        }
    }

    #[test]
    fn a_matching_session_is_bound() {
        assert_eq!(decide(Some(OURS), OURS), SessionBinding::Bound);
    }

    #[test]
    fn a_null_paired_device_is_unbound() {
        // The common case: signed in before the trusted-device cookie existed.
        // The server never revisits the field, so this can only be fixed by a
        // fresh sign-in.
        assert_eq!(decide(None, OURS), SessionBinding::Unbound);
    }

    #[test]
    fn a_session_bound_to_another_device_is_unbound() {
        // Survives re-pairing: revocation deletes sessions bound to the revoked
        // device, so one bound to a *different* device is left behind.
        assert_eq!(decide(Some(THEIRS), OURS), SessionBinding::Unbound);
    }

    #[test]
    fn binding_is_compared_exactly() {
        // No prefix or case leniency: a near-miss is a different device.
        assert_eq!(
            decide(Some("CMU0N11FC000DTOBWEX5TEQFG"), OURS),
            SessionBinding::Unbound
        );
        assert_eq!(
            decide(Some("cmu0n11fc000dtobwex5teqf"), OURS),
            SessionBinding::Unbound
        );
        assert_eq!(decide(Some(""), OURS), SessionBinding::Unbound);
    }

    #[test]
    fn the_probe_reads_the_session_binding_field() {
        // Guards the shape this depends on: `data.session.pairedDeviceId`.
        let body = r#"{"data":{"user":{"id":"u1"},"session":{"id":"s1","pairedDeviceId":"cmu0n11fc000dtobwex5teqfg"}}}"#;
        let parsed: MeEnvelope = serde_json::from_str(body).unwrap();
        assert_eq!(
            parsed.data.session.unwrap().paired_device_id.as_deref(),
            Some(OURS)
        );
    }

    #[test]
    fn an_absent_session_is_not_a_stale_binding() {
        // Nobody signed in yet. Treating that as stale would clear a cookie
        // that does not exist and imply a problem there is none.
        let body = r#"{"data":{"user":{"id":"u1"},"session":null}}"#;
        let parsed: MeEnvelope = serde_json::from_str(body).unwrap();
        assert!(parsed.data.session.is_none());
    }

    #[test]
    fn a_session_without_the_field_parses_as_unbound() {
        // An older server that does not send it must not crash the probe.
        let body = r#"{"data":{"user":{"id":"u1"},"session":{"id":"s1"}}}"#;
        let parsed: MeEnvelope = serde_json::from_str(body).unwrap();
        assert!(parsed.data.session.unwrap().paired_device_id.is_none());
    }
}
