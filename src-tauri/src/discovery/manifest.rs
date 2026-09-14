//! Deciding whether an address is an Arciin server.
//!
//! Nothing reached over the network is trusted until it has been through
//! `validate_manifest`. This is the only place an address becomes a
//! "verified server".

use serde::Serialize;
use url::Url;

use crate::address::{self, candidate_origins, friendly_address, origin_string};
use crate::error::{from_code, AppError};
use crate::http::{build_client, error_from_response, json_from_response, DISCOVERY_TIMEOUT};
use crate::protocol::{validate_manifest, DiscoveryManifest, DISCOVERY_PATH};

/// A server that answered with a manifest we accepted.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VerifiedServer {
    pub server_id: String,
    pub name: String,
    /// Canonical origin actually used to reach it.
    pub base_url: String,
    /// What to show on the card: `192.168.1.50`, `https://arciin.example.com`.
    pub display_address: String,
    pub protocol_version: u32,
    pub pairing_supported: bool,
    pub pairing_available: bool,
    /// Whether this server can serve computer backup *and* speaks a version
    /// this build implements. Negotiated, never assumed.
    pub backup_supported: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

impl VerifiedServer {
    fn from_manifest(origin: &Url, manifest: DiscoveryManifest) -> Self {
        Self {
            server_id: manifest.server_id,
            name: manifest.instance_name,
            base_url: origin_string(origin),
            display_address: friendly_address(origin),
            protocol_version: manifest.protocol_version,
            pairing_supported: manifest.pairing_supported,
            pairing_available: manifest.pairing_available,
            backup_supported: manifest
                .capabilities
                .as_ref()
                .and_then(|c| c.computer_backup.as_ref())
                .is_some_and(|c| c.usable()),
            version: manifest.version,
        }
    }
}

/// Fetch and validate the manifest at exactly one origin.
pub async fn verify_origin(origin: &Url) -> Result<VerifiedServer, AppError> {
    let client = build_client(DISCOVERY_TIMEOUT)?;
    let url = origin
        .join(DISCOVERY_PATH)
        .map_err(|_| from_code("ADDRESS_INVALID"))?;

    tracing::info!(origin = %origin_string(origin), "fetching discovery manifest");
    let started = std::time::Instant::now();

    let response = client.get(url).send().await?;
    if !response.status().is_success() {
        return Err(error_from_response(response).await);
    }

    let manifest: DiscoveryManifest = json_from_response(response).await?;
    validate_manifest(&manifest)?;
    let server_id = manifest.server_id.clone();
    let protocol_version = manifest.protocol_version;
    let server = VerifiedServer::from_manifest(origin, manifest);

    tracing::info!(
        origin = %origin_string(origin),
        server_id = %server_id,
        protocol_version,
        backup_supported = server.backup_supported,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "discovery manifest verified"
    );

    Ok(server)
}

/// Resolve a typed address to a verified server.
///
/// Candidate origins are tried in order (see `address::candidate_origins`).
/// A *protocol* rejection stops the walk immediately — that address really is
/// an Arciin server, just an incompatible one, and silently falling through to
/// another scheme would replace a precise message with "unreachable".
pub async fn verify_address(input: &str) -> Result<VerifiedServer, AppError> {
    let candidates = candidate_origins(input)?;
    let mut last: Option<AppError> = None;

    for origin in &candidates {
        match verify_origin(origin).await {
            Ok(server) => return Ok(server),
            Err(err) => {
                let fatal = matches!(
                    err.code.as_str(),
                    "DEVICE_PROTOCOL_UNSUPPORTED" | "NOT_ARCIIN" | "INSTANCE_NOT_READY"
                );
                if fatal {
                    return Err(err);
                }
                last = Some(err);
            }
        }
    }

    Err(last.unwrap_or_else(|| from_code("UNREACHABLE")))
}

/// Re-check a saved server before its credential is used.
///
/// LAN addresses get reassigned. If `192.168.1.50` was server ABC yesterday
/// and is server XYZ today, sending ABC's device credential to XYZ would hand
/// a secret to a machine that has no right to it. So the identity is
/// re-verified on every connection, and a mismatch is a hard stop.
pub async fn verify_expected_server(
    base_url: &str,
    expected_server_id: &str,
) -> Result<VerifiedServer, AppError> {
    let origin = Url::parse(base_url).map_err(|_| from_code("ADDRESS_INVALID"))?;
    if !address::is_same_origin(&origin, base_url) {
        return Err(from_code("ADDRESS_INVALID"));
    }

    let server = verify_origin(&origin).await?;
    if server.server_id != expected_server_id {
        tracing::warn!(
            origin = %origin_string(&origin),
            expected = expected_server_id,
            found = %server.server_id,
            "server identity changed at a saved address"
        );
        return Err(from_code("SERVER_ID_MISMATCH"));
    }
    Ok(server)
}
