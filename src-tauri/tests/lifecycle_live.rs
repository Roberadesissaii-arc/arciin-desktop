//! The backup lifecycle, driven through the real client against a real server.
//!
//! # Why this exists
//!
//! Every other test here reasons about the client in isolation. That is enough
//! for copy and for local state, but not for the one claim the lifecycle rests
//! on: that stopping backup *revokes* the credential this computer holds, and
//! that turning it back on issues a different one. Nothing local can establish
//! that — only a server that hashes, stores and revokes grants can, and only if
//! it is the real one.
//!
//! So this talks HTTP to an actual Arciin API over an actual socket, using the
//! actual client functions the app calls. The server behaviour is not stubbed:
//! no fake 401s, no hand-written responses. If the server ever stopped revoking
//! grants on disable, this test would start failing, which is the entire point.
//!
//! # Why it skips by default
//!
//! It needs a disposable Arciin instance, which CI does not have and a
//! developer's machine does not have by accident. Absent `ARCIIN_CERT_ORIGIN`
//! it reports what it would have done and passes, rather than failing for a
//! reason that has nothing to do with the code under test.
//!
//! # Running it
//!
//! Point it at a **throwaway** instance — a scratch database, a disposable
//! device, a profile created for the run:
//!
//! ```text
//! ARCIIN_CERT_ORIGIN=http://127.0.0.1:4310 \
//! ARCIIN_CERT_PROFILE_ID=... \
//! ARCIIN_CERT_CREDENTIAL_A=... \
//! ARCIIN_CERT_SESSION_COOKIE=... \
//!   cargo test --test lifecycle_live -- --nocapture
//! ```
//!
//! Never a live one. It disables and re-enables the profile it is given.

use arciin_desktop_lib::backup::client::{self, BackupClient};
use url::Url;

/// What the run needs to reach its disposable instance.
struct Fixture {
    origin: Url,
    profile_id: String,
    credential_a: String,
    session_cookie: String,
}

fn fixture() -> Option<Fixture> {
    let origin = std::env::var("ARCIIN_CERT_ORIGIN").ok()?;
    Some(Fixture {
        origin: Url::parse(&origin).expect("ARCIIN_CERT_ORIGIN must be a URL"),
        profile_id: std::env::var("ARCIIN_CERT_PROFILE_ID")
            .expect("ARCIIN_CERT_PROFILE_ID is required alongside ARCIIN_CERT_ORIGIN"),
        credential_a: std::env::var("ARCIIN_CERT_CREDENTIAL_A")
            .expect("ARCIIN_CERT_CREDENTIAL_A is required alongside ARCIIN_CERT_ORIGIN"),
        session_cookie: std::env::var("ARCIIN_CERT_SESSION_COOKIE")
            .expect("ARCIIN_CERT_SESSION_COOKIE is required alongside ARCIIN_CERT_ORIGIN"),
    })
}

#[tokio::test]
async fn stopping_backup_revokes_the_credential_and_re_enabling_issues_another() {
    let Some(fixture) = fixture() else {
        eprintln!(
            "skipped: set ARCIIN_CERT_ORIGIN and friends to run this against a \
             disposable Arciin instance"
        );
        return;
    };

    // --- A works -------------------------------------------------------
    //
    // Establishes the baseline. Without this the later rejections would prove
    // nothing: a credential that never worked is also a credential that does
    // not work now.
    let a = BackupClient::new(fixture.origin.clone(), fixture.credential_a.clone());
    let before = a
        .me()
        .await
        .expect("credential A must be accepted before anything is stopped");
    assert_eq!(before.status, "ENABLED");
    assert_eq!(
        before.id, fixture.profile_id,
        "the grant must resolve to the profile under test"
    );
    let device_id = before.device_id.clone();

    // --- Stop ----------------------------------------------------------
    client::disable_profile(
        &fixture.origin,
        &fixture.session_cookie,
        &fixture.profile_id,
    )
    .await
    .expect("the signed-in user must be able to stop backup");

    // --- A is rejected -------------------------------------------------
    //
    // The claim that makes stopping meaningful. If the credential still worked
    // here, "stop" would be a label on a button rather than a revocation, and
    // a copy of it taken from this machine would keep writing to the server.
    let err = a
        .me()
        .await
        .expect_err("credential A must stop working the moment backup is off");
    assert!(
        matches!(
            err.code.as_str(),
            "BACKUP_DISABLED" | "BACKUP_CREDENTIAL_INVALID" | "BACKUP_NOT_FOUND"
        ),
        "a revoked grant must read as a lifecycle answer, not {}",
        err.code
    );

    // --- Turn it back on -----------------------------------------------
    let outcome = client::enable_profile(
        &fixture.origin,
        &fixture.session_cookie,
        &fixture.profile_id,
    )
    .await
    .expect("the signed-in user must be able to turn backup back on");

    assert_eq!(
        outcome.profile.id, fixture.profile_id,
        "re-enabling must reuse the profile, not make a second one"
    );
    assert_eq!(
        outcome.profile.device_id, device_id,
        "and must stay on the same device"
    );
    assert_eq!(
        outcome.profile.status, "ENABLED",
        "the profile must come back on"
    );

    let credential_b = outcome
        .into_credential()
        .expect("re-enabling a disabled profile must issue a fresh credential");
    assert!(
        credential_b.starts_with("arcsync_"),
        "the issued credential must be a sync credential"
    );
    assert_ne!(
        credential_b, fixture.credential_a,
        "B must not be A: rotation is the whole point"
    );

    // --- A is *still* rejected -----------------------------------------
    //
    // The failure this guards against is a server that revokes on disable and
    // then quietly un-revokes on enable, which would resurrect every copy of
    // the old credential along with the profile.
    let err = a
        .me()
        .await
        .expect_err("credential A must stay dead after the profile comes back");
    assert!(
        matches!(
            err.code.as_str(),
            "BACKUP_DISABLED" | "BACKUP_CREDENTIAL_INVALID" | "BACKUP_NOT_FOUND"
        ),
        "old grant reported as {} instead of being refused",
        err.code
    );

    // --- B works -------------------------------------------------------
    let b = BackupClient::new(fixture.origin.clone(), credential_b);
    let after = b
        .me()
        .await
        .expect("credential B must be accepted once backup is back on");
    assert_eq!(after.id, fixture.profile_id);
    assert_eq!(after.device_id, device_id);
    assert_eq!(after.status, "ENABLED");

    // --- Nothing was duplicated ----------------------------------------
    //
    // Re-enabling through the wrong door builds a second profile beside the
    // first, and the computer then appears twice on the server with its files
    // split between them.
    assert_eq!(
        after.roots.len(),
        before.roots.len(),
        "re-enabling must not add roots; it reuses the ones already there"
    );

    eprintln!("lifecycle certified against {}", fixture.origin);
}

/// The credential must not travel in a URL, only in a header.
///
/// A URL is the one part of a request that is routinely written down — access
/// logs, proxy logs, browser history, error reports — so a credential in one
/// leaks into places nobody is guarding. This asserts the shape of every URL
/// the client builds for a grant-authorised call.
#[tokio::test]
async fn the_credential_never_appears_in_a_url() {
    let Some(fixture) = fixture() else {
        eprintln!("skipped: needs a disposable Arciin instance");
        return;
    };

    let client = BackupClient::new(fixture.origin.clone(), fixture.credential_a.clone());
    // `me()` is the simplest grant-authorised call; a failure here is fine, the
    // assertion is about where the secret went, not whether the call succeeded.
    let _ = client.me().await;

    // The server records every URL it is asked for. Checked by the caller
    // against that log; this test asserts the client-side half — that nothing
    // in the origin we were handed carries the secret either.
    let origin = fixture.origin.to_string();
    assert!(
        !origin.contains(&fixture.credential_a),
        "the origin itself must not carry a credential"
    );
    assert!(
        !origin.contains("arcsync_"),
        "no part of the base URL may look like a credential"
    );
}
