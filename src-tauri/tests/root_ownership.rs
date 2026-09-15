//! Who owns a protected folder, and how the server is told.
//!
//! The server used to infer that a folder was protected from it having files
//! in it. That reads a legitimately empty folder as unprotected, and — worse —
//! reads a folder no computer is backing up any more as still protected,
//! because the old files are still there. A real folder sat in that second
//! state: the server reported it "Up to date" while no machine held any record
//! of it and not one byte had ever been sent.
//!
//! Ownership is now stated rather than guessed. This computer acknowledges the
//! folders it holds, and repeats the list on every heartbeat. These assert the
//! two rules that make the claim trustworthy: it is only ever made *after* the
//! folder is committed locally, and it stops being made the moment the folder
//! is removed — and nothing weaker, such as an unplugged drive, ends it.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use arciin_desktop_lib::backup::client::BackupClient;
use arciin_desktop_lib::backup::manager::acknowledge_owned_roots;
use arciin_desktop_lib::backup::protocol::BackupHealth;
use arciin_desktop_lib::backup::store::{Profile, Root, RootStatus, SyncStore};

const SERVER: &str = "3f2504e0-4f89-41d3-9a0c-0305e82c3301";
const CREDENTIAL: &str = "arcsync_test_credential_value";

// --- A server that records what it was told ------------------------------
//
// Small on purpose. A mock library would be a new dependency in a release
// freeze, and all these need is the request line and the body.

#[derive(Debug, Clone)]
struct Received {
    path: String,
    body: String,
}

struct Stub {
    origin: url::Url,
    received: Arc<Mutex<Vec<Received>>>,
}

impl Stub {
    fn start() -> Self {
        Self::with_failures(0)
    }

    fn with_failures(failures: usize) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let received = Arc::new(Mutex::new(Vec::new()));
        let failures_left = Arc::new(AtomicUsize::new(failures));

        let recorder = Arc::clone(&received);
        let failing = Arc::clone(&failures_left);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                handle(stream, &recorder, &failing);
            }
        });

        Self {
            origin: url::Url::parse(&format!("http://127.0.0.1:{port}")).unwrap(),
            received,
        }
    }

    fn client(&self) -> BackupClient {
        BackupClient::new(self.origin.clone(), CREDENTIAL.to_string())
    }

    fn requests(&self) -> Vec<Received> {
        self.received.lock().unwrap().clone()
    }

    fn paths(&self) -> Vec<String> {
        self.requests().into_iter().map(|r| r.path).collect()
    }
}

fn handle(mut stream: TcpStream, received: &Arc<Mutex<Vec<Received>>>, failing: &Arc<AtomicUsize>) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());

    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    let path = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_string();

    let mut length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = value.trim().parse().unwrap_or(0);
        }
    }

    let mut body = vec![0u8; length];
    if length > 0 {
        let _ = reader.read_exact(&mut body);
    }

    received.lock().unwrap().push(Received {
        path,
        body: String::from_utf8_lossy(&body).into_owned(),
    });

    // Burn one scheduled failure, if the test asked for any.
    let should_fail = failing
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
            if left == 0 {
                None
            } else {
                Some(left - 1)
            }
        })
        .is_ok();

    let response = if should_fail {
        "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}"
    } else {
        "HTTP/1.1 201 Created\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}"
    };
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

// --- Local state ---------------------------------------------------------

struct Harness {
    _dir: tempfile::TempDir,
    store: SyncStore,
    root_dir: std::path::PathBuf,
}

impl Harness {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = SyncStore::open(&dir.path().join("state")).unwrap();
        store.save_profile(&profile(false)).unwrap();
        let root_dir = dir.path().join("Protected");
        std::fs::create_dir_all(&root_dir).unwrap();
        Self {
            _dir: dir,
            store,
            root_dir,
        }
    }

    /// Persist a root the way the real code does, and return its id.
    fn persist(&self, id: &str, name: &str) -> String {
        let path = self.root_dir.join(name);
        std::fs::create_dir_all(&path).unwrap();
        self.store
            .save_root(
                SERVER,
                &Root {
                    id: id.into(),
                    kind: "CUSTOM".into(),
                    display_name: name.into(),
                    local_path: path,
                    enabled: true,
                    status: RootStatus::Active,
                },
            )
            .unwrap();
        id.to_string()
    }

    fn owned(&self) -> Vec<String> {
        self.store.owned_root_identifiers(SERVER).unwrap()
    }

    fn awaiting(&self) -> Vec<String> {
        self.store
            .roots_awaiting_acknowledgement(SERVER)
            .unwrap()
            .into_iter()
            .map(|root| root.id)
            .collect()
    }
}

fn profile(paused: bool) -> Profile {
    Profile {
        server_id: SERVER.into(),
        profile_id: "profile-1".into(),
        device_id: "device-1".into(),
        user_id: "user-1".into(),
        paused,
        enabled: true,
    }
}

// --- Acknowledgement -----------------------------------------------------

#[tokio::test]
async fn a_folder_is_acknowledged_only_once_it_is_stored_locally() {
    // The ordering rule. Until the row is committed there is nothing to claim,
    // and claiming first is what let the server believe a folder was protected
    // by a computer that had no record of it.
    let h = Harness::new();
    let stub = Stub::start();

    assert!(
        h.awaiting().is_empty(),
        "nothing is owed to the server before a folder is stored"
    );
    acknowledge_owned_roots(&h.store, &stub.client(), SERVER).await;
    assert!(
        stub.requests().is_empty(),
        "an unstored folder must never be announced"
    );

    h.persist("root-1", "Photos");
    assert_eq!(h.awaiting(), vec!["root-1".to_string()]);

    acknowledge_owned_roots(&h.store, &stub.client(), SERVER).await;
    assert_eq!(stub.paths(), vec!["/api/backup/roots".to_string()]);
}

#[tokio::test]
async fn a_folder_that_never_persisted_is_never_announced() {
    // Stands in for a failed write: the row is simply not there. The server
    // must hear nothing at all rather than hearing about a folder this
    // computer will not be backing up.
    let h = Harness::new();
    let stub = Stub::start();

    h.persist("root-1", "Photos");
    // A second folder that "failed to persist" — never saved.
    acknowledge_owned_roots(&h.store, &stub.client(), SERVER).await;

    assert_eq!(stub.requests().len(), 1, "only the stored folder is sent");
    let body = &stub.requests()[0].body;
    assert!(body.contains("Photos"), "and it is the one that was stored");
}

#[tokio::test]
async fn acknowledgement_is_not_repeated_once_it_has_been_recorded() {
    // Idempotent in the sense that matters locally: a folder already announced
    // is not announced again on every reconcile. The server upserts, so a
    // repeat would be harmless — but it would also be a request per folder per
    // pass, forever.
    let h = Harness::new();
    let stub = Stub::start();
    h.persist("root-1", "Photos");

    acknowledge_owned_roots(&h.store, &stub.client(), SERVER).await;
    acknowledge_owned_roots(&h.store, &stub.client(), SERVER).await;
    acknowledge_owned_roots(&h.store, &stub.client(), SERVER).await;

    assert_eq!(stub.requests().len(), 1, "announced exactly once");
    assert!(h.awaiting().is_empty());
}

#[tokio::test]
async fn a_refused_acknowledgement_is_retried_on_the_next_pass() {
    // The folder is still being backed up whether or not the server took the
    // claim, so the claim has to survive a failure and be repeated.
    let h = Harness::new();
    let stub = Stub::with_failures(1);
    h.persist("root-1", "Photos");

    acknowledge_owned_roots(&h.store, &stub.client(), SERVER).await;
    assert_eq!(
        h.awaiting(),
        vec!["root-1".to_string()],
        "a refusal leaves the claim outstanding"
    );

    acknowledge_owned_roots(&h.store, &stub.client(), SERVER).await;
    assert!(h.awaiting().is_empty(), "and the next pass makes it");
    assert_eq!(stub.requests().len(), 2);
}

#[tokio::test]
async fn an_empty_protected_folder_is_acknowledged_like_any_other() {
    // The reason this exists. An empty folder has no files to infer protection
    // from, and is protected exactly as much as a full one.
    let h = Harness::new();
    let stub = Stub::start();
    h.persist("root-empty", "Empty");

    acknowledge_owned_roots(&h.store, &stub.client(), SERVER).await;

    assert_eq!(stub.requests().len(), 1);
    assert_eq!(
        h.owned().len(),
        1,
        "and it counts as owned with nothing in it"
    );
}

#[tokio::test]
async fn a_removed_folder_is_never_acknowledged() {
    // The Games case, in reverse: acknowledging marks the root protected
    // server-side, so announcing one the user has removed would silently
    // protect it again.
    let h = Harness::new();
    let stub = Stub::start();
    h.persist("root-1", "Photos");
    h.store.disable_root(SERVER, "root-1").unwrap();

    acknowledge_owned_roots(&h.store, &stub.client(), SERVER).await;

    assert!(stub.requests().is_empty());
    assert!(h.awaiting().is_empty());
}

#[tokio::test]
async fn nothing_in_an_acknowledgement_reveals_where_the_folder_is() {
    let h = Harness::new();
    let stub = Stub::start();
    h.persist("root-1", "Photos");

    acknowledge_owned_roots(&h.store, &stub.client(), SERVER).await;

    let body = &stub.requests()[0].body;
    let local = h.root_dir.join("Photos").to_string_lossy().to_string();
    assert!(!body.contains(&local), "the absolute path must not be sent");
    assert!(!body.contains(":\\"), "no drive-qualified path: {body}");
    assert!(!body.contains(":/"), "no drive-qualified path: {body}");
    assert!(
        !body.to_lowercase().contains("users"),
        "no profile directory: {body}"
    );
    assert!(body.contains("sourcePathIdentifier"), "identifier is sent");
    assert!(!body.contains(CREDENTIAL), "and never the credential");
}

// --- Ownership in the heartbeat ------------------------------------------

#[tokio::test]
async fn the_heartbeat_carries_every_folder_this_computer_holds() {
    let h = Harness::new();
    let stub = Stub::start();
    h.persist("root-1", "Photos");
    h.persist("root-2", "Papers");

    stub.client()
        .heartbeat(BackupHealth::UpToDate, None, Some(&h.owned()))
        .await
        .unwrap();

    let body = &stub.requests()[0].body;
    assert!(body.contains("ownedRootSourceIdentifiers"), "{body}");
    for identifier in h.owned() {
        assert!(
            body.contains(&identifier),
            "{identifier} missing from {body}"
        );
    }
    assert!(!body.contains(":\\"), "no path may ride along: {body}");
}

#[tokio::test]
async fn a_heartbeat_with_nothing_to_say_omits_the_field_entirely() {
    // An absent field and an empty list mean different things. "I could not
    // read my own configuration" must not arrive looking like "I own nothing",
    // which would invite the server to disable every folder.
    let stub = Stub::start();

    stub.client()
        .heartbeat(BackupHealth::UpToDate, None, None)
        .await
        .unwrap();

    let body = &stub.requests()[0].body;
    assert!(!body.contains("ownedRootSourceIdentifiers"), "{body}");
    assert!(body.contains("health"), "the old fields still go: {body}");
}

// --- What ownership survives ---------------------------------------------

#[test]
fn a_folder_on_a_disconnected_drive_is_still_owned() {
    let h = Harness::new();
    h.persist("root-1", "Photos");
    h.store
        .set_root_status(SERVER, "root-1", RootStatus::Unavailable)
        .unwrap();

    assert_eq!(
        h.owned().len(),
        1,
        "an unreadable folder is still this computer's to look after"
    );
}

#[test]
fn a_folder_held_for_review_is_still_owned() {
    let h = Harness::new();
    h.persist("root-1", "Photos");
    h.store
        .set_root_status(SERVER, "root-1", RootStatus::SafetyHold)
        .unwrap();

    assert_eq!(h.owned().len(), 1);
}

#[test]
fn pausing_backup_does_not_give_up_any_folder() {
    let h = Harness::new();
    h.persist("root-1", "Photos");
    h.store.save_profile(&profile(true)).unwrap();

    assert_eq!(h.owned().len(), 1, "paused is not removed");
}

#[test]
fn removing_a_folder_is_the_only_thing_that_ends_ownership() {
    let h = Harness::new();
    h.persist("root-1", "Photos");
    h.persist("root-2", "Papers");
    assert_eq!(h.owned().len(), 2);

    h.store.disable_root(SERVER, "root-1").unwrap();

    assert_eq!(h.owned().len(), 1, "and only the removed one goes");
}

#[test]
fn ownership_is_rebuilt_from_disk_after_a_restart() {
    // Nothing about ownership lives in memory. A machine that has just booted,
    // with no watcher registered and no network, still knows what it owns.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state");
    let protected = dir.path().join("Photos");
    std::fs::create_dir_all(&protected).unwrap();

    let before = {
        let store = SyncStore::open(&path).unwrap();
        store.save_profile(&profile(false)).unwrap();
        store
            .save_root(
                SERVER,
                &Root {
                    id: "root-1".into(),
                    kind: "CUSTOM".into(),
                    display_name: "Photos".into(),
                    local_path: protected,
                    enabled: true,
                    status: RootStatus::Active,
                },
            )
            .unwrap();
        store.owned_root_identifiers(SERVER).unwrap()
    };

    let reopened = SyncStore::open(&path).unwrap();
    assert_eq!(
        reopened.owned_root_identifiers(SERVER).unwrap(),
        before,
        "the same folders, by the same identifiers"
    );
    assert_eq!(before.len(), 1);
}

#[test]
fn the_identifier_for_a_folder_never_changes() {
    // The server keys on it. A value that drifted between saves would create a
    // second root for the same folder on the next reconcile.
    let h = Harness::new();
    h.persist("root-1", "Photos");
    let first = h.owned();

    // Saved again, as a status change or a rename would.
    h.persist("root-1", "Photos");

    assert_eq!(h.owned(), first);
    assert_eq!(
        h.store.roots(SERVER).unwrap().len(),
        1,
        "and re-saving never duplicates the folder"
    );
}

#[test]
fn two_folders_never_share_an_identifier() {
    let h = Harness::new();
    h.persist("root-1", "Photos");
    h.persist("root-2", "Papers");

    let owned = h.owned();
    assert_eq!(owned.len(), 2);
    assert_ne!(owned[0], owned[1]);
}

#[test]
fn an_upgraded_install_recovers_identifiers_it_never_stored() {
    // Schema 4 kept no identifier. The value is not lost: it is a function of
    // the server id, the kind and the path, all of which the old row already
    // has — so an upgrade recomputes rather than asking the server.
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let protected = dir.path().join("Photos");
    std::fs::create_dir_all(&protected).unwrap();

    let expected = {
        let store = SyncStore::open(&state).unwrap();
        store.save_profile(&profile(false)).unwrap();
        store
            .save_root(
                SERVER,
                &Root {
                    id: "root-1".into(),
                    kind: "CUSTOM".into(),
                    display_name: "Photos".into(),
                    local_path: protected,
                    enabled: true,
                    status: RootStatus::Active,
                },
            )
            .unwrap();
        store.owned_root_identifiers(SERVER).unwrap()
    };
    assert_eq!(expected.len(), 1);

    // Put the row back the way version 4 left it.
    {
        let conn = rusqlite::Connection::open(state.join("backup-state.db")).unwrap();
        conn.execute("UPDATE roots SET source_path_identifier = NULL", [])
            .unwrap();
        conn.pragma_update(None, "user_version", 4i64).unwrap();
    }

    let reopened = SyncStore::open(&state).unwrap();
    assert_eq!(
        reopened.owned_root_identifiers(SERVER).unwrap(),
        expected,
        "the upgrade reproduces exactly what the server was given"
    );
    assert_eq!(
        reopened
            .roots_awaiting_acknowledgement(SERVER)
            .unwrap()
            .len(),
        1,
        "and an upgraded install has still never told the server it owns them"
    );
}
