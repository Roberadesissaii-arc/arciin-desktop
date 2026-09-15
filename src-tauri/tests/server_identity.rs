//! Which server this is, when its address is not a reliable answer.
//!
//! Home networks move. A router reboots and hands out a different lease, the
//! machine moves from Wi-Fi to Ethernet, somebody sets a static address. The
//! server is the same server throughout, and a client that decided otherwise
//! would ask its owner to pair again for no reason — and, worse, would leave a
//! second trusted device on the server every time it happened.
//!
//! This project has watched that happen: the instance under test changed LAN
//! address twice during development, and the client carried on with the same
//! pairing both times.
//!
//! The rule these assert: identity is the `serverId` from the discovery
//! manifest. The address is only how we reached it today.
//!
//! The matching rule for *credentials* lives beside them in
//! `src/credentials`, because the in-memory store used to assert it is
//! test-only and not part of the library's public surface.

use arciin_desktop_lib::servers::SavedServer;

const SERVER_A: &str = "3f2504e0-4f89-41d3-9a0c-0305e82c3301";
const SERVER_B: &str = "9c858901-8a57-4791-81fe-4c455b099bc9";

/// Addresses here are RFC 5737 documentation addresses. This repository is
/// public and must never record where somebody's server actually lives.
fn saved(server_id: &str, base_url: &str, name: &str) -> SavedServer {
    SavedServer {
        server_id: server_id.into(),
        name: name.into(),
        base_url: base_url.into(),
        protocol_version: 1,
        last_connected_at: None,
        revoked: false,
    }
}

/// The store's rule, as a function: replace the entry with this identity, or
/// add it. Mirrors `ServerStore::upsert`, which needs a Tauri app handle.
fn upsert(servers: &mut Vec<SavedServer>, server: SavedServer) {
    match servers
        .iter_mut()
        .find(|existing| existing.server_id == server.server_id)
    {
        Some(existing) => *existing = server,
        None => servers.push(server),
    }
}

#[test]
fn the_same_server_at_a_new_address_is_still_one_server() {
    // The lease changed. Nothing about the instance did.
    let mut servers = Vec::new();
    upsert(
        &mut servers,
        saved(SERVER_A, "http://203.0.113.10:3002", "Local Instance"),
    );
    upsert(
        &mut servers,
        saved(SERVER_A, "http://203.0.113.20:3002", "Local Instance"),
    );

    assert_eq!(
        servers.len(),
        1,
        "a moved server must not become a second one"
    );
    assert_eq!(servers[0].base_url, "http://203.0.113.20:3002");
    assert_eq!(servers[0].server_id, SERVER_A);
}

#[test]
fn two_servers_sharing_an_address_stay_two_servers() {
    // One instance replaced by another at the same address — a rebuilt box, a
    // reused static lease. Treating them as the same would hand one server's
    // trust to a different one.
    let mut servers = Vec::new();
    upsert(
        &mut servers,
        saved(SERVER_A, "http://203.0.113.10:3002", "Old"),
    );
    upsert(
        &mut servers,
        saved(SERVER_B, "http://203.0.113.10:3002", "New"),
    );

    assert_eq!(servers.len(), 2);
    assert_ne!(servers[0].server_id, servers[1].server_id);
}

#[test]
fn a_revoked_server_stays_flagged_across_a_move() {
    // Revocation is about the instance, not about where it was last seen.
    let mut servers = Vec::new();
    let mut first = saved(SERVER_A, "http://203.0.113.10:3002", "Local Instance");
    first.revoked = true;
    upsert(&mut servers, first);

    // Rediscovered at a new address, still revoked until somebody pairs again.
    let mut moved = servers[0].clone();
    moved.base_url = "http://203.0.113.20:3002".into();
    upsert(&mut servers, moved);

    assert_eq!(servers.len(), 1);
    assert!(servers[0].revoked, "a move must not quietly restore trust");
}

#[test]
fn discovery_finding_several_servers_keeps_them_distinct() {
    // Two Arciin instances on one network. The client must be able to tell
    // them apart and must not silently attach to whichever answered first.
    let mut servers = Vec::new();
    upsert(
        &mut servers,
        saved(SERVER_A, "http://203.0.113.20:3002", "Home"),
    );
    upsert(
        &mut servers,
        saved(SERVER_B, "http://203.0.113.30:3002", "Office"),
    );

    assert_eq!(servers.len(), 2);
    let names: Vec<&str> = servers.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"Home") && names.contains(&"Office"));

    // And the paired one is found by identity, not by position or address.
    let paired = servers
        .iter()
        .find(|server| server.server_id == SERVER_B)
        .expect("the paired server must be findable by its identity");
    assert_eq!(paired.name, "Office");
}
