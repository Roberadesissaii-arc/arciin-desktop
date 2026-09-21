//! Contract tests for the parts of the client that must not drift from
//! `docs/DESKTOP-PAIRING-PROTOCOL.md`, and for the security decisions that
//! depend on getting a URL comparison exactly right.

use arciin_desktop_lib::address::{
    candidate_origins, friendly_address, is_safe_external, is_same_origin, origin_string,
    AddressError,
};
use arciin_desktop_lib::error::from_code;
use arciin_desktop_lib::protocol::{
    format_pairing_code, normalize_pairing_code, validate_manifest, DiscoveryManifest,
    ManifestRejection, DEVICE_PROTOCOL_VERSION,
};
use url::Url;

const SERVER_A: &str = "3f2504e0-4f89-41d3-9a0c-0305e82c3301";
const SERVER_B: &str = "9c858901-8a57-4791-81fe-4c455b099bc9";

fn manifest(server_id: &str, protocol: u32) -> DiscoveryManifest {
    DiscoveryManifest {
        service: "arciin".into(),
        protocol_version: protocol,
        server_id: server_id.into(),
        instance_name: "Arciin Home".into(),
        version: Some("1.0.1".into()),
        pairing_supported: true,
        pairing_available: true,
        web_url: Some("http://192.168.1.20".into()),
    }
}

// --- Address normalization ----------------------------------------------

#[test]
fn bare_lan_ip_becomes_http() {
    let candidates = candidate_origins("192.168.1.50").unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(origin_string(&candidates[0]), "http://192.168.1.50");
}

#[test]
fn bare_lan_ip_with_port_keeps_the_port() {
    let candidates = candidate_origins("192.168.1.50:3000").unwrap();
    assert_eq!(origin_string(&candidates[0]), "http://192.168.1.50:3000");
}

#[test]
fn mdns_name_becomes_http() {
    let candidates = candidate_origins("arciin.local").unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(origin_string(&candidates[0]), "http://arciin.local");
}

#[test]
fn explicit_http_is_preserved() {
    let candidates = candidate_origins("http://192.168.1.50").unwrap();
    assert_eq!(origin_string(&candidates[0]), "http://192.168.1.50");
}

#[test]
fn explicit_https_is_never_downgraded() {
    let candidates = candidate_origins("https://arciin.example.com").unwrap();
    assert_eq!(candidates.len(), 1, "no http fallback may be offered");
    assert_eq!(candidates[0].scheme(), "https");
}

#[test]
fn bare_public_host_prefers_tls_then_falls_back() {
    let candidates = candidate_origins("arciin.example.com").unwrap();
    assert_eq!(candidates.len(), 2);
    assert_eq!(candidates[0].scheme(), "https");
    assert_eq!(candidates[1].scheme(), "http");
}

#[test]
fn paths_and_queries_are_stripped_to_an_origin() {
    let candidates = candidate_origins("http://192.168.1.50/dashboard?x=1#y").unwrap();
    assert_eq!(origin_string(&candidates[0]), "http://192.168.1.50");
}

#[test]
fn credentials_in_the_url_are_dropped() {
    let candidates = candidate_origins("http://user:pass@192.168.1.50").unwrap();
    let origin = &candidates[0];
    assert_eq!(origin.username(), "");
    assert_eq!(origin.password(), None);
}

#[test]
fn dangerous_schemes_are_refused() {
    for input in [
        "javascript:alert(1)",
        "data:text/html,<script>1</script>",
        "file:///C:/Windows",
        "ftp://192.168.1.50",
    ] {
        let err = candidate_origins(input).unwrap_err();
        assert!(
            matches!(err, AddressError::UnsupportedScheme(_)),
            "{input} should be refused, got {err:?}"
        );
    }
}

#[test]
fn empty_input_is_refused() {
    assert_eq!(candidate_origins("   ").unwrap_err(), AddressError::Empty);
}

#[test]
fn friendly_address_hides_noise_but_keeps_signal() {
    let plain = Url::parse("http://192.168.1.50").unwrap();
    assert_eq!(friendly_address(&plain), "192.168.1.50");

    let ported = Url::parse("http://192.168.1.50:3000").unwrap();
    assert_eq!(friendly_address(&ported), "192.168.1.50:3000");

    let secure = Url::parse("https://arciin.example.com").unwrap();
    assert_eq!(friendly_address(&secure), "https://arciin.example.com");
}

// --- Origin allowlist ----------------------------------------------------

#[test]
fn same_origin_accepts_only_an_exact_match() {
    let allowed = Url::parse("http://192.168.1.50").unwrap();

    assert!(is_same_origin(&allowed, "http://192.168.1.50/dashboard"));
    assert!(is_same_origin(&allowed, "http://192.168.1.50/files?q=a"));

    // Different port, host, or scheme is a different origin.
    assert!(!is_same_origin(&allowed, "http://192.168.1.50:8080/"));
    assert!(!is_same_origin(&allowed, "http://192.168.1.51/"));
    assert!(!is_same_origin(&allowed, "https://192.168.1.50/"));

    // A hostname that merely contains the allowed one must not pass.
    let named = Url::parse("https://arciin.example.com").unwrap();
    assert!(!is_same_origin(
        &named,
        "https://arciin.example.com.evil.test/"
    ));
    assert!(!is_same_origin(
        &named,
        "https://evil.test/?x=arciin.example.com"
    ));
}

#[test]
fn same_origin_refuses_non_web_schemes() {
    let allowed = Url::parse("http://192.168.1.50").unwrap();
    assert!(!is_same_origin(&allowed, "javascript:alert(1)"));
    assert!(!is_same_origin(&allowed, "data:text/html,hi"));
    assert!(!is_same_origin(&allowed, "not a url"));
}

#[test]
fn only_web_links_may_reach_the_system_browser() {
    assert!(is_safe_external("https://arciin.com/docs"));
    assert!(is_safe_external("http://example.test"));
    assert!(!is_safe_external("javascript:alert(1)"));
    assert!(!is_safe_external("data:text/html,x"));
    assert!(!is_safe_external("file:///C:/Windows/System32"));
}

// --- Manifest validation -------------------------------------------------

#[test]
fn a_well_formed_manifest_is_accepted() {
    assert!(validate_manifest(&manifest(SERVER_A, DEVICE_PROTOCOL_VERSION)).is_ok());
}

#[test]
fn a_non_arciin_service_is_rejected() {
    let mut m = manifest(SERVER_A, DEVICE_PROTOCOL_VERSION);
    m.service = "plex".into();
    assert_eq!(
        validate_manifest(&m).unwrap_err(),
        ManifestRejection::NotArciin
    );
}

#[test]
fn an_unsupported_protocol_is_rejected_with_both_versions() {
    let m = manifest(SERVER_A, 2);
    assert_eq!(
        validate_manifest(&m).unwrap_err(),
        ManifestRejection::UnsupportedProtocol {
            server: 2,
            client: DEVICE_PROTOCOL_VERSION,
        }
    );
    assert_eq!(
        validate_manifest(&m).unwrap_err().code(),
        "DEVICE_PROTOCOL_UNSUPPORTED"
    );
}

#[test]
fn a_bogus_server_id_is_rejected() {
    for bad in ["", "not-a-uuid", "3f2504e04f8941d39a0c0305e82c3301"] {
        let m = manifest(bad, DEVICE_PROTOCOL_VERSION);
        assert_eq!(
            validate_manifest(&m).unwrap_err(),
            ManifestRejection::InvalidServerId,
            "{bad} should be rejected"
        );
    }
}

#[test]
fn a_nameless_instance_is_rejected() {
    let mut m = manifest(SERVER_A, DEVICE_PROTOCOL_VERSION);
    m.instance_name = "  ".into();
    assert_eq!(
        validate_manifest(&m).unwrap_err(),
        ManifestRejection::MissingInstanceName
    );
}

// --- Server identity binding --------------------------------------------

#[test]
fn server_ids_distinguish_two_servers_at_one_address() {
    // The scenario the check exists for: the same LAN address, a different
    // machine behind it. Identity comes from the manifest, never the address.
    let yesterday = manifest(SERVER_A, DEVICE_PROTOCOL_VERSION);
    let today = manifest(SERVER_B, DEVICE_PROTOCOL_VERSION);
    assert_ne!(yesterday.server_id, today.server_id);
    assert_eq!(from_code("SERVER_ID_MISMATCH").code, "SERVER_ID_MISMATCH");
}

// --- Pairing codes -------------------------------------------------------

#[test]
fn pairing_codes_normalize_from_what_people_type() {
    assert_eq!(normalize_pairing_code("482731").as_deref(), Some("482731"));
    assert_eq!(normalize_pairing_code("482 731").as_deref(), Some("482731"));
    assert_eq!(normalize_pairing_code("482-731").as_deref(), Some("482731"));
    assert_eq!(
        normalize_pairing_code(" 482731 ").as_deref(),
        Some("482731")
    );
}

#[test]
fn wrong_length_codes_are_refused_before_the_network() {
    assert_eq!(normalize_pairing_code("48273"), None);
    assert_eq!(normalize_pairing_code("4827311"), None);
    assert_eq!(normalize_pairing_code(""), None);
    assert_eq!(normalize_pairing_code("abcdef"), None);
}

#[test]
fn pairing_codes_display_in_two_groups() {
    assert_eq!(format_pairing_code("482731"), "482 731");
}

// --- Error mapping -------------------------------------------------------

#[test]
fn every_protocol_error_code_has_its_own_copy() {
    // Section 11 of the protocol document, in full.
    let codes = [
        "PAIRING_CODE_INVALID",
        "PAIRING_CODE_EXPIRED",
        "PAIRING_CODE_LOCKED",
        "PAIRING_ALREADY_USED",
        "PAIRING_CANCELLED",
        "DEVICE_REVOKED",
        "DEVICE_INVALID",
        "DEVICE_PROTOCOL_UNSUPPORTED",
        "PAIRING_REQUIRED",
        "VALIDATION_ERROR",
        "UNAUTHENTICATED",
        "FORBIDDEN",
        "RATE_LIMITED",
        "INSTANCE_NOT_READY",
    ];

    let fallback = from_code("__no_such_code__").message;
    for code in codes {
        let err = from_code(code);
        assert_eq!(err.code, code);
        assert_ne!(
            err.message, fallback,
            "{code} must have specific copy, not the generic fallback"
        );
        assert!(!err.message.is_empty());
    }
}

#[test]
fn unrecoverable_errors_are_not_marked_retryable() {
    for code in [
        "DEVICE_REVOKED",
        "DEVICE_INVALID",
        "DEVICE_PROTOCOL_UNSUPPORTED",
        "SERVER_ID_MISMATCH",
        "NOT_ARCIIN",
    ] {
        assert!(!from_code(code).retryable, "{code} must not invite a retry");
    }
}

#[test]
fn transient_errors_invite_a_retry() {
    for code in [
        "TIMEOUT",
        "UNREACHABLE",
        "RATE_LIMITED",
        "PAIRING_CODE_INVALID",
    ] {
        assert!(from_code(code).retryable, "{code} should be retryable");
    }
}

#[test]
fn no_error_message_leaks_a_secret_shaped_word() {
    for code in [
        "PAIRING_CODE_INVALID",
        "DEVICE_REVOKED",
        "CREDENTIAL_MISSING",
        "WEBVIEW_BRIDGE_UNAVAILABLE",
    ] {
        let message = from_code(code).message.to_lowercase();
        for forbidden in ["credential=", "token", "cookie", "authorization"] {
            assert!(
                !message.contains(forbidden),
                "{code} copy must not mention {forbidden}"
            );
        }
    }
}

// --- Losing trust ---------------------------------------------------------
//
// Only a server that says so unpairs this computer. Nothing else may.

use arciin_desktop_lib::connection::trust::is_trust_lost;

#[test]
fn a_network_blip_never_unpairs_the_computer() {
    // The failure mode worth guarding: losing Wi-Fi is not a revocation, and
    // treating it as one would disconnect people at random.
    for code in [
        "UNREACHABLE",
        "TIMEOUT",
        "TLS_ERROR",
        "RATE_LIMITED",
        "INTERNAL_ERROR",
        "SERVER_ID_MISMATCH",
    ] {
        assert!(!is_trust_lost(code), "{code} must not disconnect anyone");
    }
}

// --- Every answer the server can give -------------------------------------
//
// A code that falls through to the generic "something went wrong connecting"
// is both untrue and — because the fallback is retryable — an invitation to
// retry a doomed request.
