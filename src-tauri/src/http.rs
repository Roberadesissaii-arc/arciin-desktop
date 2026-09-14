//! The one HTTP client the app talks to Arciin with.
//!
//! Every request has a finite timeout, so no UI state can wait forever.
//! TLS validation is never relaxed: `danger_accept_invalid_certs` appears
//! nowhere in this crate, and plain HTTP stays available because an
//! unencrypted LAN install is a supported Arciin deployment.

use std::time::Duration;

use serde::Deserialize;

use crate::error::{from_code, AppError};

/// Discovery is a small unauthenticated GET on a LAN; it should be quick.
pub const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(6);

/// Pairing runs an Argon2id verification server-side, so it needs more room.
pub const PAIRING_TIMEOUT: Duration = Duration::from_secs(20);

/// Bootstrap is a hash lookup plus a session write.
pub const BOOTSTRAP_TIMEOUT: Duration = Duration::from_secs(15);

/// Cap on how much of a response body we will read before deciding it is not
/// a manifest, so a hostile LAN responder cannot stream at us indefinitely.
const MAX_BODY_BYTES: usize = 64 * 1024;

/// The server's error envelope: `{ "error": { "code", "message" } }`.
#[derive(Debug, Deserialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

#[derive(Debug, Deserialize)]
struct ErrorBody {
    code: String,
}

/// Build the shared client.
///
/// `User-Agent` deliberately keeps a desktop browser token: the web app's
/// proxy routes phone user agents to the mobile PWA, and this window is the
/// desktop surface.
pub fn build_client(timeout: Duration) -> Result<reqwest::Client, AppError> {
    reqwest::Client::builder()
        .timeout(timeout)
        .connect_timeout(Duration::from_secs(5))
        // Redirects are followed inside an origin only; a discovery probe that
        // bounces to another host is not that host's manifest.
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!(
            "ArciinDesktop/",
            env!("CARGO_PKG_VERSION"),
            " (Windows; desktop)"
        ))
        .build()
        .map_err(|err| {
            tracing::error!(error = %err, "failed to build http client");
            AppError::internal("HTTP client could not be created.")
        })
}

/// Read a response body with a size cap.
async fn read_capped(response: reqwest::Response) -> Result<Vec<u8>, AppError> {
    let bytes = response.bytes().await?;
    if bytes.len() > MAX_BODY_BYTES {
        return Err(from_code("INVALID_MANIFEST"));
    }
    Ok(bytes.to_vec())
}

/// Turn a non-2xx response into the server's own error code where one is
/// present, falling back to a status-derived code.
///
/// The body is parsed for `error.code` only. No message from the server is
/// echoed to the user, so a compromised or spoofed endpoint cannot put
/// arbitrary text on screen.
pub async fn error_from_response(response: reqwest::Response) -> AppError {
    let status = response.status();
    let body = match read_capped(response).await {
        Ok(body) => body,
        Err(err) => return err,
    };

    if let Ok(envelope) = serde_json::from_slice::<ErrorEnvelope>(&body) {
        tracing::warn!(status = status.as_u16(), code = %envelope.error.code, "arciin returned an error");
        return from_code(&envelope.error.code);
    }

    let code = match status.as_u16() {
        401 | 403 => "DEVICE_INVALID",
        404 => "NOT_ARCIIN",
        409 => "INSTANCE_NOT_READY",
        429 => "RATE_LIMITED",
        _ => "UNREACHABLE",
    };
    tracing::warn!(
        status = status.as_u16(),
        code,
        "arciin returned an unparsed error"
    );
    from_code(code)
}

/// Decode a successful JSON response.
pub async fn json_from_response<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
) -> Result<T, AppError> {
    let body = read_capped(response).await?;
    serde_json::from_slice(&body).map_err(|err| {
        tracing::warn!(error = %err, "response was not the expected shape");
        from_code("INVALID_MANIFEST")
    })
}
