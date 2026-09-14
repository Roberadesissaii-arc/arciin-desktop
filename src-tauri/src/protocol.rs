//! The Arciin device protocol as defined by the server.
//!
//! Source of truth: `arciin-main/docs/DESKTOP-PAIRING-PROTOCOL.md`
//! (server feature branch `feature/device-pairing`).
//!
//! Nothing here may be invented. Every constant, path and error code below
//! appears verbatim in that document or in
//! `arciin-main/packages/config/src/device-pairing.ts`.

use serde::{Deserialize, Serialize};

/// `ARCIIN_DEVICE_PROTOCOL_VERSION`. Independent of the Arciin app version.
pub const DEVICE_PROTOCOL_VERSION: u32 = 1;

/// The `service` discriminator every genuine manifest must carry.
pub const DISCOVERY_SERVICE: &str = "arciin";

/// Public, unauthenticated discovery manifest.
pub const DISCOVERY_PATH: &str = "/.well-known/arciin";

/// Unauthenticated, rate-limited pairing claim.
pub const PAIR_PATH: &str = "/api/devices/pair";

/// Device bootstrap. Proves "this is a previously paired device".
pub const DEVICE_SESSION_PATH: &str = "/api/devices/session";

/// HttpOnly cookie the server sets on a successful bootstrap.
pub const TRUSTED_DEVICE_COOKIE: &str = "arciin_trusted_device";

/// DNS-SD service type advertised by the server (when Avahi is present).
pub const MDNS_SERVICE_TYPE: &str = "_arciin._tcp.local.";

/// This client reports itself as `windows` / `desktop`.
pub const PLATFORM_WINDOWS: &str = "windows";
pub const DEVICE_TYPE_DESKTOP: &str = "desktop";

/// Server-side cap; a longer name is rejected with `VALIDATION_ERROR`.
pub const DEVICE_NAME_MAX_LENGTH: usize = 80;

/// Pairing codes are exactly six digits.
pub const PAIRING_CODE_DIGITS: usize = 6;

/// The public discovery manifest.
///
/// Only the fields the desktop client is allowed to act on are modelled.
/// Unknown fields are ignored so a newer server does not break an older client.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryManifest {
    pub service: String,
    pub protocol_version: u32,
    pub server_id: String,
    pub instance_name: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub pairing_supported: bool,
    #[serde(default)]
    pub pairing_available: bool,
    #[serde(default)]
    pub web_url: Option<String>,
    /// Optional. Absent on a server that predates computer backup, which must
    /// keep working exactly as before.
    #[serde(default)]
    pub capabilities: Option<crate::backup::protocol::ServerCapabilities>,
}

/// Why a candidate address was not accepted as an Arciin server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestRejection {
    /// `service` was not `arciin` — something else answered on this address.
    NotArciin,
    /// The server speaks a protocol this build does not implement.
    UnsupportedProtocol { server: u32, client: u32 },
    /// `serverId` missing or not a plausible stable identifier.
    InvalidServerId,
    /// `instanceName` missing.
    MissingInstanceName,
}

impl ManifestRejection {
    /// Stable identifier surfaced to the UI layer.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotArciin => "NOT_ARCIIN",
            Self::UnsupportedProtocol { .. } => "DEVICE_PROTOCOL_UNSUPPORTED",
            Self::InvalidServerId => "INVALID_MANIFEST",
            Self::MissingInstanceName => "INVALID_MANIFEST",
        }
    }
}

/// A `serverId` is a UUID. Matching the server's own
/// `PUBLIC_SERVER_ID_RE` shape check keeps a hostile LAN responder from
/// handing us an identifier we would then persist and compare against.
fn looks_like_server_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (i, b) in bytes.iter().enumerate() {
        let ok = match i {
            8 | 13 | 18 | 23 => *b == b'-',
            _ => b.is_ascii_hexdigit(),
        };
        if !ok {
            return false;
        }
    }
    true
}

/// Validate a decoded manifest before any of it is trusted or persisted.
///
/// A response arriving from an arbitrary LAN address is untrusted input: it is
/// only an Arciin server once every one of these checks passes.
pub fn validate_manifest(manifest: &DiscoveryManifest) -> Result<(), ManifestRejection> {
    if manifest.service != DISCOVERY_SERVICE {
        return Err(ManifestRejection::NotArciin);
    }
    if manifest.protocol_version != DEVICE_PROTOCOL_VERSION {
        return Err(ManifestRejection::UnsupportedProtocol {
            server: manifest.protocol_version,
            client: DEVICE_PROTOCOL_VERSION,
        });
    }
    if !looks_like_server_id(&manifest.server_id) {
        return Err(ManifestRejection::InvalidServerId);
    }
    if manifest.instance_name.trim().is_empty() {
        return Err(ManifestRejection::MissingInstanceName);
    }
    Ok(())
}

/// Normalize user input to the six digits the server expects.
/// Mirrors `normalizeDevicePairingCode` on the server.
pub fn normalize_pairing_code(input: &str) -> Option<String> {
    let digits: String = input.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() != PAIRING_CODE_DIGITS {
        return None;
    }
    Some(digits)
}

/// Display grouping used by the server UI: `482731` -> `482 731`.
pub fn format_pairing_code(code: &str) -> String {
    let digits: String = code.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() != PAIRING_CODE_DIGITS {
        return digits;
    }
    format!("{} {}", &digits[..3], &digits[3..])
}
