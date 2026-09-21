//! One error type crossing the IPC boundary, and the mapping from the
//! server's stable error codes to the sentence a person actually reads.

use serde::Serialize;

use crate::address::AddressError;
use crate::protocol::ManifestRejection;

/// What the frontend receives when a command fails.
///
/// `code` is stable and safe to branch on; `message` is display copy.
/// Neither ever carries a credential, a pairing code or a cookie.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AppError {
    pub code: String,
    pub message: String,
    /// Set when retrying the same action could plausibly succeed.
    pub retryable: bool,
}

impl AppError {
    pub fn new(code: &str, message: &str, retryable: bool) -> Self {
        Self {
            code: code.to_string(),
            message: message.to_string(),
            retryable,
        }
    }

    /// An unexpected internal failure. The underlying detail is logged, not
    /// shown, so nothing sensitive leaks into the UI.
    pub fn internal(message: &str) -> Self {
        Self::new("INTERNAL_ERROR", message, true)
    }
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for AppError {}

/// Friendly copy for every error code in the protocol's table, plus the
/// transport failures that only the client can observe.
///
/// Codes come from `docs/DESKTOP-PAIRING-PROTOCOL.md` §11.
pub fn friendly(code: &str) -> (&'static str, bool) {
    match code {
        // --- Pairing (§11) ---
        "PAIRING_CODE_INVALID" => ("That pairing code isn't correct.", true),
        "PAIRING_CODE_EXPIRED" => (
            "That pairing code expired. Generate a new one from Settings \u{2192} Devices.",
            true,
        ),
        "PAIRING_CODE_LOCKED" => (
            "Too many attempts. Generate a new pairing code from Settings \u{2192} Devices.",
            true,
        ),
        "PAIRING_ALREADY_USED" => ("That pairing code has already been used.", true),
        "PAIRING_CANCELLED" => (
            "That pairing code was cancelled. Generate a new one from Settings \u{2192} Devices.",
            true,
        ),
        "PAIRING_REQUIRED" => ("This server requires a paired device.", false),

        // --- Device trust (§11) ---
        "DEVICE_REVOKED" => (
            "This computer is no longer connected to this Arciin server.",
            false,
        ),
        "DEVICE_INVALID" => (
            "This computer is no longer recognised by this Arciin server.",
            false,
        ),
        "DEVICE_PROTOCOL_UNSUPPORTED" => (
            "Update Arciin Desktop or your Arciin server \u{2014} they speak different versions.",
            false,
        ),

        // --- Server-side generic (§11) ---
        "INSTANCE_NOT_READY" => (
            "This Arciin server hasn't finished setup yet. Complete setup in a browser first.",
            true,
        ),
        "RATE_LIMITED" => ("Too many attempts. Wait a moment and try again.", true),
        "VALIDATION_ERROR" => ("Arciin rejected those details.", false),
        "UNAUTHENTICATED" => ("You need to sign in on the server first.", false),
        "FORBIDDEN" => (
            "This account can't manage devices. Ask an owner or admin.",
            false,
        ),

        // --- Discovery / transport (client-observed) ---
        "NOT_ARCIIN" => (
            "That address answered, but it isn't an Arciin server.",
            false,
        ),
        "INVALID_MANIFEST" => (
            "That address answered with something Arciin Desktop couldn't read.",
            false,
        ),
        "SERVER_ID_MISMATCH" => (
            "This address now points to a different Arciin server. You'll need to pair again.",
            false,
        ),
        "UNREACHABLE" => (
            "Couldn't reach that address. Check the server is on and on this network.",
            true,
        ),
        "TIMEOUT" => ("That server didn't answer in time.", true),
        "TLS_ERROR" => (
            "Couldn't establish a secure connection to that server.",
            false,
        ),
        "ADDRESS_EMPTY" => ("Enter a server address.", false),
        "ADDRESS_INVALID" => ("That doesn't look like a server address.", false),
        "ADDRESS_SCHEME_UNSUPPORTED" => ("Arciin Desktop only connects over http or https.", false),
        "PAIRING_CODE_MALFORMED" => ("A pairing code is six digits.", false),
        "CREDENTIAL_MISSING" => (
            "This computer has no saved credential for that server. Pair again.",
            false,
        ),
        "CREDENTIAL_STORE_ERROR" => ("Windows Credential Manager couldn't be reached.", true),
        "SERVER_NOT_SAVED" => ("That server isn't saved on this computer.", false),
        "WEBVIEW_BRIDGE_UNAVAILABLE" => (
            "Arciin Desktop couldn't hand the secure session to the app window.",
            true,
        ),

        _ => ("Something went wrong connecting to Arciin.", true),
    }
}

/// Build a user-facing error from a stable code.
pub fn from_code(code: &str) -> AppError {
    let (message, retryable) = friendly(code);
    AppError::new(code, message, retryable)
}

impl From<AddressError> for AppError {
    fn from(value: AddressError) -> Self {
        from_code(value.code())
    }
}

impl From<ManifestRejection> for AppError {
    fn from(value: ManifestRejection) -> Self {
        from_code(value.code())
    }
}

/// Classify a transport failure without ever surfacing the raw error string,
/// which can contain the full request URL.
impl From<reqwest::Error> for AppError {
    fn from(value: reqwest::Error) -> Self {
        let code = if value.is_timeout() {
            "TIMEOUT"
        } else if value.is_connect() {
            // reqwest folds TLS handshake failures into connect errors.
            let text = value.to_string().to_ascii_lowercase();
            if text.contains("certificate") || text.contains("tls") || text.contains("handshake") {
                "TLS_ERROR"
            } else {
                "UNREACHABLE"
            }
        } else if value.is_decode() {
            "INVALID_MANIFEST"
        } else {
            "UNREACHABLE"
        };
        from_code(code)
    }
}
