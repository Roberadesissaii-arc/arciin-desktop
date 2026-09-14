//! Optional mDNS / DNS-SD browse for `_arciin._tcp.local.`
//!
//! The server reports mDNS support as **partial** — Docker bridge networking
//! does not reliably carry multicast, and Avahi may simply not be installed.
//! So every failure here is swallowed: no error reaches the user, and the
//! manual address path is always offered. Finding nothing is a normal result.
//!
//! A responder is only ever a *hint*. Whatever it claims about itself is
//! ignored; the address it gives is put through the same manifest
//! verification as something typed by hand.

use std::collections::HashSet;
use std::time::Duration;

use mdns_sd::{ServiceDaemon, ServiceEvent};

use crate::protocol::MDNS_SERVICE_TYPE;

/// How long to listen before giving up. Short enough that the UI does not
/// feel stuck, long enough for a responder on a quiet LAN to answer.
pub const BROWSE_WINDOW: Duration = Duration::from_secs(4);

/// Cap on how many responders we will follow up on, so a noisy or hostile
/// network cannot make the app issue unbounded verification requests.
const MAX_CANDIDATES: usize = 12;

/// Collect candidate `scheme://host:port` origins advertised on the network.
///
/// Returns an empty list on any failure, including mDNS being unavailable.
pub fn browse() -> Vec<String> {
    match browse_inner() {
        Ok(found) => found,
        Err(err) => {
            // Expected on plenty of machines. Informational, never an error.
            tracing::info!(error = %err, "mDNS browse unavailable; manual address still works");
            Vec::new()
        }
    }
}

fn browse_inner() -> Result<Vec<String>, mdns_sd::Error> {
    let daemon = ServiceDaemon::new()?;
    let receiver = daemon.browse(MDNS_SERVICE_TYPE)?;

    let deadline = std::time::Instant::now() + BROWSE_WINDOW;
    let mut origins = Vec::new();
    let mut seen = HashSet::new();

    while std::time::Instant::now() < deadline && origins.len() < MAX_CANDIDATES {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let Ok(event) = receiver.recv_timeout(remaining) else {
            break;
        };

        let ServiceEvent::ServiceResolved(info) = event else {
            continue;
        };

        let port = info.get_port();
        for address in info.get_addresses() {
            // A responder advertising a port of 0 is malformed; skip it
            // rather than building a URL that cannot be fetched.
            if port == 0 {
                continue;
            }
            let host = if address.is_ipv6() {
                format!("[{address}]")
            } else {
                address.to_string()
            };
            let origin = if port == 80 {
                format!("http://{host}")
            } else {
                format!("http://{host}:{port}")
            };
            if seen.insert(origin.clone()) {
                tracing::info!(origin = %origin, "mDNS responder advertised an Arciin service");
                origins.push(origin);
            }
        }
    }

    // Best-effort; the daemon shutting down cleanly is not worth an error.
    let _ = daemon.shutdown();
    Ok(origins)
}
