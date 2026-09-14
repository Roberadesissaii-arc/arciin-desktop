//! Finding Arciin servers.
//!
//! Two independent paths, both ending at the same verification step:
//! mDNS (a convenience that may find nothing) and a typed address (always
//! available). Neither is trusted until `manifest::verify_origin` accepts it.
//!
//! Subnet scanning is deliberately not implemented.

pub mod manifest;
pub mod mdns;

use crate::error::AppError;

pub use manifest::{verify_address, verify_expected_server, verify_origin, VerifiedServer};

/// Browse the network, then verify every responder.
///
/// Responders that fail verification are dropped quietly: something else on
/// the LAN answering `_arciin._tcp` is not an error the user needs to see.
pub async fn discover() -> Result<Vec<VerifiedServer>, AppError> {
    // `browse` blocks on a multicast socket, so keep it off the async runtime.
    let candidates = tokio::task::spawn_blocking(mdns::browse)
        .await
        .unwrap_or_default();

    // One host commonly advertises several addresses — an IPv4 plus link-local
    // and ULA IPv6 — and the ones this machine cannot route to each burn their
    // full connect timeout. Verified serially that is seconds of dead air on
    // the search screen, so they are checked concurrently and the results
    // collected in the advertised order.
    let checks: Vec<_> = candidates
        .into_iter()
        .filter_map(|origin| {
            let parsed = url::Url::parse(&origin).ok()?;
            Some(tokio::spawn(async move {
                (origin, verify_origin(&parsed).await)
            }))
        })
        .collect();

    let mut servers: Vec<VerifiedServer> = Vec::new();
    for check in checks {
        let Ok((origin, result)) = check.await else {
            continue;
        };
        match result {
            Ok(server) => {
                // One server can answer on several addresses; show it once.
                if !servers.iter().any(|s| s.server_id == server.server_id) {
                    servers.push(server);
                }
            }
            Err(err) => {
                tracing::info!(origin = %origin, code = %err.code, "mDNS responder was not an Arciin server");
            }
        }
    }

    tracing::info!(count = servers.len(), "network discovery finished");
    Ok(servers)
}
