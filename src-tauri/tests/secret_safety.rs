//! Where the backup credential is allowed to go, and where it is not.
//!
//! The rule is narrow enough to state in a sentence: the `ArciinSync`
//! credential goes from the server into Windows Credential Manager, and from
//! there into one `Authorization` header. It reaches no log, no database, no
//! URL, and no part of the UI.
//!
//! Stating it is easy; keeping it true across changes is not, because each of
//! those leaks is one careless line away — a `tracing::info!` that interpolates
//! the wrong variable, a debug field added to a struct that is serialised to
//! the renderer. These tests fail on the line, not on the incident.

use std::path::{Path, PathBuf};

fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// Every `.rs` file under `src/`, with its path.
fn rust_sources() -> Vec<(PathBuf, String)> {
    fn walk(dir: &Path, out: &mut Vec<(PathBuf, String)>) {
        for entry in std::fs::read_dir(dir).expect("src must be readable") {
            let path = entry.expect("readable entry").path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                let text = std::fs::read_to_string(&path).expect("source must be UTF-8");
                out.push((path, text));
            }
        }
    }
    let mut out = Vec::new();
    walk(&src_dir(), &mut out);
    assert!(!out.is_empty(), "found no sources to audit");
    out
}

/// A line with its string literals blanked out.
///
/// The distinction that matters: `tracing::info!("credential stored")` says the
/// word in a message, which is fine and useful. `tracing::info!(?credential)`
/// prints the secret. Only the second survives this.
fn without_string_literals(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut in_string = false;
    let mut escaped = false;
    for ch in line.chars() {
        match ch {
            _ if escaped => escaped = false,
            '\\' if in_string => escaped = true,
            '"' => in_string = !in_string,
            _ if !in_string => out.push(ch),
            _ => {}
        }
    }
    out
}

#[test]
fn no_logging_call_ever_interpolates_a_secret() {
    // Words that name something that must never be printed. Checked against
    // the line with its message text removed, so saying "credential" in a
    // message is fine and passing one as a field is not.
    const FORBIDDEN: [&str; 6] = [
        "credential",
        "secret",
        "password",
        "cookie",
        "authorization",
        "arcsync",
    ];
    // Names that merely *contain* a forbidden word but carry no secret.
    const ALLOWED: [&str; 4] = [
        "credential_issued",
        "credentials",
        "credential_store",
        "session_cookie_header",
    ];

    let mut offences = Vec::new();
    for (path, text) in rust_sources() {
        for (number, line) in text.lines().enumerate() {
            if !line.contains("tracing::") {
                continue;
            }
            let mut code = without_string_literals(line).to_lowercase();
            for allowed in ALLOWED {
                code = code.replace(allowed, "");
            }
            for word in FORBIDDEN {
                if code.contains(word) {
                    offences.push(format!(
                        "{}:{} passes `{word}` to a logging macro",
                        path.display(),
                        number + 1
                    ));
                }
            }
        }
    }
    assert!(
        offences.is_empty(),
        "secrets must never reach a log:\n{}",
        offences.join("\n")
    );
}

/// The body of one `impl` block, by brace depth.
fn impl_block<'a>(source: &'a str, header: &str) -> &'a str {
    let start = source
        .find(header)
        .unwrap_or_else(|| panic!("{header} not found"));
    let body = &source[start + header.len()..];
    let mut depth = 1usize;
    for (index, ch) in body.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &body[..index];
                }
            }
            _ => {}
        }
    }
    panic!("{header} is unbalanced");
}

#[test]
fn the_sync_credential_is_attached_in_exactly_one_place() {
    // Structural, not stylistic. One attachment point is what makes the claim
    // "it only ever travels in an Authorization header" checkable at all; a
    // second one is where a credential ends up in a query string or a log.
    let client = std::fs::read_to_string(src_dir().join("backup").join("client.rs"))
        .expect("client.rs must exist");

    let body = impl_block(&client, "impl BackupClient {");
    let uses: Vec<&str> = body
        .lines()
        .filter(|line| without_string_literals(line).contains("self.credential"))
        .map(str::trim)
        .collect();

    assert_eq!(
        uses.len(),
        1,
        "the client must read its credential in exactly one place, found: {uses:?}"
    );
    assert!(
        uses[0].contains("SYNC_AUTH_SCHEME"),
        "its one use must be building the Authorization header, not: {}",
        uses[0]
    );
}

#[test]
fn the_issued_credential_leaves_its_carrier_exactly_once() {
    // `EnableOutcome` is the only thing that ever holds a freshly issued
    // credential in the client. It exists to be consumed once, at the point
    // the secret is handed to Windows Credential Manager — so the field must
    // have no accessor that could hand it anywhere else.
    let client = std::fs::read_to_string(src_dir().join("backup").join("client.rs"))
        .expect("client.rs must exist");

    let body = impl_block(&client, "impl EnableOutcome {");
    let uses: Vec<&str> = body
        .lines()
        .filter(|line| without_string_literals(line).contains("self.credential"))
        .map(str::trim)
        .collect();

    assert_eq!(
        uses.len(),
        1,
        "the issued credential must be readable in exactly one place, found: {uses:?}"
    );
    assert!(
        body.contains("pub fn into_credential(self)"),
        "and that place must consume the outcome, so it cannot be read twice"
    );
}

#[test]
fn no_url_is_ever_built_from_a_credential() {
    // The failure this guards: a credential in a path or query string. URLs are
    // written down everywhere — access logs, proxies, error reports — so one
    // there leaks into places nobody is guarding.
    let client = std::fs::read_to_string(src_dir().join("backup").join("client.rs"))
        .expect("client.rs must exist");

    for (number, line) in client.lines().enumerate() {
        let joins_a_url = line.contains(".join(") || line.contains("format!(\"/");
        if joins_a_url {
            assert!(
                !line.to_lowercase().contains("credential"),
                "backup/client.rs:{} builds a URL from a credential: {}",
                number + 1,
                line.trim()
            );
        }
    }
}

#[test]
fn the_local_database_never_stores_a_credential() {
    use arciin_desktop_lib::backup::store::{Profile, Root, SyncStore};

    // A credential-shaped value, written nowhere on purpose. If the store ever
    // grew a column for one, the obvious way to fill it would be from the
    // profile — so a profile is what gets saved here.
    const SENTINEL: &str = "arcsync_ThisMustNeverReachDisk0000000000000";

    let dir = tempfile::tempdir().expect("temp dir");
    let store = SyncStore::open(dir.path()).expect("store opens");
    let server_id = "3f2504e0-4f89-41d3-9a0c-0305e82c3301";

    store
        .save_profile(&Profile {
            server_id: server_id.into(),
            profile_id: "profile-1".into(),
            device_id: "device-1".into(),
            user_id: "user-1".into(),
            paused: false,
            enabled: true,
        })
        .expect("profile saves");
    store
        .save_root(
            server_id,
            &Root {
                id: "root-1".into(),
                kind: "CUSTOM".into(),
                display_name: "TestBackup".into(),
                local_path: std::path::PathBuf::from(r"D:\Profiles\TestUser\TestBackup"),
                enabled: true,
            },
        )
        .expect("root saves");
    drop(store);

    // Every byte the store wrote, WAL included — a value can sit in the
    // write-ahead log long after it left the main file.
    let mut bytes = Vec::new();
    for entry in std::fs::read_dir(dir.path()).expect("state dir readable") {
        let path = entry.expect("entry").path();
        if path.is_file() {
            bytes.extend(std::fs::read(&path).expect("state file readable"));
        }
    }
    let text = String::from_utf8_lossy(&bytes);

    assert!(!text.contains(SENTINEL), "the sentinel reached disk");
    assert!(
        !text.contains("arcsync_"),
        "something credential-shaped reached the local database"
    );
    assert!(
        !text.to_lowercase().contains("credential"),
        "the local database has a credential column; it must not"
    );
}

#[test]
fn what_the_renderer_receives_carries_no_credential() {
    use arciin_desktop_lib::backup::engine::EngineStatus;
    use arciin_desktop_lib::backup::manager::{BackupState, Lifecycle, ProtectedRootView};

    // The guard that matters most, because this struct is the *only* backup
    // data that crosses into JavaScript. A secret could only arrive there by
    // being added here, so this asserts the shape rather than a value.
    let state = BackupState {
        enabled: true,
        lifecycle: Lifecycle::Active,
        activation: None,
        last_backup_at: Some("2026-09-14T20:00:00Z".into()),
        status: Some(EngineStatus {
            health: "SYNCING".into(),
            files_synced: 12,
            files_outstanding: 3,
            files_failed: 0,
            bytes_synced: 1024,
            bytes_outstanding: 2048,
            paused: false,
            last_error: None,
        }),
        roots: vec![ProtectedRootView {
            id: "root-1".into(),
            kind: "CUSTOM".into(),
            display_name: "TestBackup".into(),
            enabled: true,
            local_path: r"D:\Profiles\TestUser\TestBackup".into(),
            local_path_exists: true,
            file_count: 12,
            pending: 0,
            failed: 0,
            bytes_synced: 1024,
        }],
    };

    let json = serde_json::to_string(&state).expect("state serialises");
    let lowered = json.to_lowercase();
    for banned in [
        "arcsync",
        "credential",
        "secret",
        "token",
        "password",
        "cookie",
        "authorization",
    ] {
        assert!(
            !lowered.contains(banned),
            "the renderer's backup state mentions `{banned}`: {json}"
        );
    }
}
