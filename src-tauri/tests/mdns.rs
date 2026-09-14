//! Does the mDNS browse actually find an Arciin service when one is present?
//!
//! This matters because the Arciin server currently does **not** advertise
//! (`mdns.advertised: false`), so in day-to-day use this code path always
//! returns an empty list — which looks identical to it being broken.
//!
//! The test publishes a service of exactly the type and shape
//! `scripts/advertise-arciin-mdns.sh` publishes, then browses for it. A pass
//! means automatic discovery starts working the moment the server advertises,
//! with no desktop change.

use std::collections::HashMap;
use std::net::UdpSocket;

use mdns_sd::{ServiceDaemon, ServiceInfo};

/// Matches `ARCIIN_MDNS_SERVICE_TYPE` plus the `.local.` domain.
const SERVICE_TYPE: &str = "_arciin._tcp.local.";

/// The web origin port this deployment actually serves on.
const PORT: u16 = 3002;

/// This machine's LAN address.
///
/// A responder that only advertises `127.0.0.1` is not announced on a real
/// interface, so the browse would never see it — the same reason the loopback
/// shortcut does not reflect how a server on the network behaves.
fn lan_address() -> Option<String> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    // No packet is sent; this just asks the routing table which local address
    // would be used to reach the LAN.
    socket.connect("192.168.1.1:80").ok()?;
    Some(socket.local_addr().ok()?.ip().to_string())
}

#[test]
fn browse_finds_an_advertised_arciin_service() {
    let Ok(responder) = ServiceDaemon::new() else {
        // No multicast socket available (locked-down CI container). The browse
        // path is designed to be harmless in exactly this case, and `browse`
        // itself is verified to return empty rather than error.
        eprintln!("skipping: mDNS daemon unavailable in this environment");
        assert!(arciin_desktop_lib::discovery::mdns::browse().is_empty());
        return;
    };

    let Some(address) = lan_address() else {
        eprintln!("skipping: no LAN address available");
        return;
    };

    // The same TXT records the server-side helper script publishes.
    let mut properties = HashMap::new();
    properties.insert("protocol".to_string(), "1".to_string());
    properties.insert("path".to_string(), "/.well-known/arciin".to_string());

    let service = ServiceInfo::new(
        SERVICE_TYPE,
        "Arciin",
        "arciin-test-host.local.",
        address.as_str(),
        PORT,
        properties,
    )
    .expect("service info should be constructible");

    if responder.register(service).is_err() {
        eprintln!("skipping: could not register a test service");
        return;
    }

    let found = arciin_desktop_lib::discovery::mdns::browse();
    let _ = responder.shutdown();

    assert!(
        found
            .iter()
            .any(|origin| origin.contains(&PORT.to_string())),
        "browse should surface the advertised service; got {found:?}"
    );
}

#[test]
fn an_advertised_service_becomes_a_fetchable_origin() {
    // Whatever a responder claims, the client only ever uses it to build an
    // origin it then verifies over HTTP. This checks the shape of that origin,
    // since a malformed one would silently drop a real server.
    let Ok(responder) = ServiceDaemon::new() else {
        eprintln!("skipping: mDNS daemon unavailable in this environment");
        return;
    };

    let Some(address) = lan_address() else {
        eprintln!("skipping: no LAN address available");
        return;
    };

    let service = ServiceInfo::new(
        SERVICE_TYPE,
        "Arciin Origin Shape",
        "arciin-shape-host.local.",
        address.as_str(),
        PORT,
        HashMap::new(),
    )
    .expect("service info should be constructible");

    if responder.register(service).is_err() {
        eprintln!("skipping: could not register a test service");
        return;
    }

    let found = arciin_desktop_lib::discovery::mdns::browse();
    let _ = responder.shutdown();

    for origin in &found {
        let parsed = url::Url::parse(origin)
            .unwrap_or_else(|err| panic!("{origin} should parse as a URL: {err}"));
        assert_eq!(parsed.scheme(), "http");
        assert!(parsed.host().is_some(), "{origin} should carry a host");
    }
}
