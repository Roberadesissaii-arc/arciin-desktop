//! Talking to the Arciin backup API.
//!
//! Two authorization schemes live here, and keeping them apart is the whole
//! point of the module:
//!
//! - **Enabling** backup needs a signed-in *user*. That session is borrowed
//!   from the WebView for one call and never stored.
//! - **Everything after that** uses `Authorization: ArciinSync <credential>`,
//!   which the OS secret store holds and which is scoped to this device and
//!   this user only.
//!
//! No function here logs a credential, a session, or a file's contents.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use url::Url;

use crate::backup::protocol as bp;
use crate::error::{from_code, AppError};
use crate::http::{build_client, json_from_response};

/// Enabling creates a profile and possibly hashes a credential; give it room.
const ENABLE_TIMEOUT: Duration = Duration::from_secs(30);
/// Small JSON calls.
const CONTROL_TIMEOUT: Duration = Duration::from_secs(20);
/// A single file transfer. Generous, because a large file on a slow LAN is
/// still making progress.
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(600);

/// One protected root, as sent when enabling.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RootRequest {
    pub kind: String,
    pub display_name: String,
    /// Opaque. Never a Windows path.
    pub source_path_identifier: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct EnableRequest<'a> {
    device_id: &'a str,
    roots: &'a [RootRequest],
}

#[derive(Debug, Deserialize)]
struct Envelope<T> {
    data: T,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EnableData {
    profile: bp::BackupProfile,
    /// Returned exactly once, when a grant is issued.
    credential: Option<String>,
    #[serde(default)]
    credential_issued: bool,
}

/// The result of enabling backup.
///
/// The credential is private: it leaves this struct only by being handed to
/// the OS credential store, and there is no accessor that returns it.
pub struct EnableOutcome {
    pub profile: bp::BackupProfile,
    credential: Option<String>,
}

impl EnableOutcome {
    /// Consume the outcome, yielding the secret once at the point it is stored.
    pub fn into_credential(self) -> Option<String> {
        self.credential
    }
}

/// Enable computer backup for this device, using the signed-in user's session.
///
/// The session is passed in already borrowed so this module never touches the
/// WebView, and the caller can guarantee it is dropped promptly.
pub async fn enable_backup(
    origin: &Url,
    session_cookie_header: &str,
    device_id: &str,
    roots: &[RootRequest],
) -> Result<EnableOutcome, AppError> {
    let url = origin
        .join(bp::PROFILES_PATH)
        .map_err(|_| from_code("ADDRESS_INVALID"))?;

    tracing::info!(
        origin = %origin.origin().ascii_serialization(),
        root_count = roots.len(),
        "enabling computer backup"
    );

    let client = build_client(ENABLE_TIMEOUT)?;
    let response = client
        .post(url)
        .header("Cookie", session_cookie_header)
        .json(&EnableRequest { device_id, roots })
        .send()
        .await?;

    if !response.status().is_success() {
        let err = backup_error(response).await;
        tracing::warn!(code = %err.code, "enabling backup was rejected");
        return Err(err);
    }

    let parsed: Envelope<EnableData> = json_from_response(response).await?;
    tracing::info!(
        profile_id = %parsed.data.profile.id,
        credential_issued = parsed.data.credential_issued,
        root_count = parsed.data.profile.roots.len(),
        "computer backup enabled"
    );

    Ok(EnableOutcome {
        profile: parsed.data.profile,
        credential: parsed.data.credential,
    })
}

/// A client bound to one server and one backup grant.
///
/// Holding the credential in one place keeps it from being threaded through
/// every call site, and means there is exactly one function that attaches it.
pub struct BackupClient {
    origin: Url,
    credential: String,
}

impl BackupClient {
    pub fn new(origin: Url, credential: String) -> Self {
        Self { origin, credential }
    }

    fn url(&self, path: &str) -> Result<Url, AppError> {
        self.origin
            .join(path)
            .map_err(|_| from_code("ADDRESS_INVALID"))
    }

    /// The one place the sync credential is attached to a request.
    fn authorize(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        builder.header(
            "Authorization",
            format!("{} {}", bp::SYNC_AUTH_SCHEME, self.credential),
        )
    }

    /// Confirm the stored credential still works, and read current state.
    ///
    /// Called on startup before anything is uploaded, so a revoked or disabled
    /// profile is discovered once rather than through a storm of failures.
    pub async fn me(&self) -> Result<bp::BackupProfile, AppError> {
        let client = build_client(CONTROL_TIMEOUT)?;
        let response = self
            .authorize(client.get(self.url(bp::ME_PATH)?))
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(backup_error(response).await);
        }
        let parsed: Envelope<bp::BackupProfile> = json_from_response(response).await?;
        Ok(parsed.data)
    }

    /// Create or update a protected root.
    pub async fn upsert_root(&self, root: &RootRequest) -> Result<bp::BackupRoot, AppError> {
        let client = build_client(CONTROL_TIMEOUT)?;
        let response = self
            .authorize(client.post(self.url(bp::ROOTS_PATH)?))
            .json(root)
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(backup_error(response).await);
        }
        let parsed: Envelope<bp::BackupRoot> = json_from_response(response).await?;
        Ok(parsed.data)
    }

    /// Stop protecting one root, on the server.
    ///
    /// The server is the authority here. Disabling only locally left the two
    /// disagreeing: the server kept accepting writes for a root this computer
    /// believed it had dropped, and the protected-folder count never moved.
    ///
    /// Reversible by design — the root is disabled, not deleted, so the files
    /// already stored stay and re-adding the same folder reactivates *this*
    /// root rather than building a second tree beside it.
    pub async fn disable_root(&self, root_id: &str) -> Result<bp::BackupRoot, AppError> {
        self.set_root_enabled(root_id, false).await
    }

    /// Protect a root again, on the server.
    pub async fn enable_root(&self, root_id: &str) -> Result<bp::BackupRoot, AppError> {
        self.set_root_enabled(root_id, true).await
    }

    async fn set_root_enabled(
        &self,
        root_id: &str,
        enabled: bool,
    ) -> Result<bp::BackupRoot, AppError> {
        let action = if enabled { "enable" } else { "disable" };
        let path = format!("{}/{root_id}/{action}", bp::ROOTS_PATH);
        let client = build_client(CONTROL_TIMEOUT)?;
        let response = self.authorize(client.post(self.url(&path)?)).send().await?;
        if !response.status().is_success() {
            return Err(backup_error(response).await);
        }
        let parsed: Envelope<bp::BackupRoot> = json_from_response(response).await?;
        Ok(parsed.data)
    }

    /// Create a folder entry.
    ///
    /// Uploading a file creates its missing parents, but an *empty* directory
    /// has no file to imply it — and the product promise is that the tree comes
    /// across intact, empty folders included.
    pub async fn create_folder(
        &self,
        sync_root_id: &str,
        client_entry_id: &str,
        relative_path: &str,
        operation_id: &str,
    ) -> Result<bp::BackupEntry, AppError> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Body<'a> {
            sync_root_id: &'a str,
            client_entry_id: &'a str,
            relative_path: &'a str,
            operation_id: &'a str,
        }

        let client = build_client(CONTROL_TIMEOUT)?;
        let response = self
            .authorize(client.post(self.url(bp::FOLDERS_PATH)?))
            .json(&Body {
                sync_root_id,
                client_entry_id,
                relative_path,
                operation_id,
            })
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(backup_error(response).await);
        }
        let parsed: Envelope<bp::BackupEntry> = json_from_response(response).await?;
        Ok(parsed.data)
    }

    /// Upload or replace a file, streamed from disk.
    ///
    /// The body is a `File` turned into a stream, so a 4 GB video costs a
    /// buffer, not 4 GB of RAM.
    pub async fn upload_file(
        &self,
        sync_root_id: &str,
        client_entry_id: &str,
        relative_path: &str,
        operation_id: &str,
        absolute_path: &std::path::Path,
    ) -> Result<bp::BackupEntry, AppError> {
        use futures_util::TryStreamExt;
        use tokio_util::io::ReaderStream;

        let file = tokio::fs::File::open(absolute_path).await.map_err(|err| {
            // The path is deliberately absent from the message: it is private.
            tracing::warn!(kind = ?err.kind(), "a file could not be opened for upload");
            from_code("BACKUP_FILE_UNREADABLE")
        })?;

        // The server keys smart-library classification off the file name, so
        // the leaf name is sent as the part filename. Nothing else about the
        // local path goes with it.
        let file_name = std::path::Path::new(relative_path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "file".to_string());

        let stream = ReaderStream::new(file).map_err(std::io::Error::other);
        let part = reqwest::multipart::Part::stream(reqwest::Body::wrap_stream(stream))
            .file_name(file_name);

        let form = reqwest::multipart::Form::new()
            .text("syncRootId", sync_root_id.to_string())
            .text("clientEntryId", client_entry_id.to_string())
            .text("relativePath", relative_path.to_string())
            .text("operationId", operation_id.to_string())
            .part("file", part);

        let client = build_client(UPLOAD_TIMEOUT)?;
        let response = self
            .authorize(client.post(self.url(bp::FILES_PATH)?))
            .multipart(form)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(backup_error(response).await);
        }
        let parsed: Envelope<bp::BackupEntry> = json_from_response(response).await?;
        Ok(parsed.data)
    }

    /// Reflect a local rename or move without re-sending the bytes.
    pub async fn move_entry(
        &self,
        sync_root_id: &str,
        client_entry_id: &str,
        relative_path: &str,
        operation_id: &str,
    ) -> Result<bp::BackupEntry, AppError> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Body<'a> {
            // Required by the route's schema even though the protocol
            // document's example omits it.
            sync_root_id: &'a str,
            relative_path: &'a str,
            operation_id: &'a str,
        }

        let client = build_client(CONTROL_TIMEOUT)?;
        let response = self
            .authorize(client.post(self.url(&bp::move_path(client_entry_id))?))
            .json(&Body {
                sync_root_id,
                relative_path,
                operation_id,
            })
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(backup_error(response).await);
        }
        let parsed: Envelope<bp::BackupEntry> = json_from_response(response).await?;
        Ok(parsed.data)
    }

    /// Record that a protected entry is gone locally.
    ///
    /// The server soft-deletes into Trash with its normal retention. This is
    /// the only deletion signal this client ever sends, and it always describes
    /// something that has *already* disappeared from Windows.
    pub async fn tombstone_entry(
        &self,
        sync_root_id: &str,
        client_entry_id: &str,
        operation_id: &str,
    ) -> Result<bp::BackupEntry, AppError> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Body<'a> {
            sync_root_id: &'a str,
            operation_id: &'a str,
        }

        let client = build_client(CONTROL_TIMEOUT)?;
        let response = self
            .authorize(client.post(self.url(&bp::tombstone_path(client_entry_id))?))
            .json(&Body {
                sync_root_id,
                operation_id,
            })
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(backup_error(response).await);
        }
        let parsed: Envelope<bp::BackupEntry> = json_from_response(response).await?;
        Ok(parsed.data)
    }

    /// Report sync health. Throttled by the caller, not by this function.
    pub async fn heartbeat(
        &self,
        health: bp::BackupHealth,
        last_error: Option<&str>,
    ) -> Result<(), AppError> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Body<'a> {
            health: &'a str,
            last_error: Option<&'a str>,
        }

        let client = build_client(CONTROL_TIMEOUT)?;
        let response = self
            .authorize(client.post(self.url(bp::HEARTBEAT_PATH)?))
            .json(&Body {
                health: health.as_str(),
                last_error,
            })
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(backup_error(response).await);
        }
        Ok(())
    }
}

/// Map a failed backup response to an `AppError`.
///
/// Backup codes get their own copy; anything else falls through to the shared
/// pairing/transport mapping so a plain network failure still reads sensibly.
async fn backup_error(response: reqwest::Response) -> AppError {
    let status = response.status();
    let body = response.bytes().await.unwrap_or_default();

    #[derive(Deserialize)]
    struct ErrorEnvelope {
        error: ErrorBody,
    }
    #[derive(Deserialize)]
    struct ErrorBody {
        code: String,
    }

    if let Ok(envelope) = serde_json::from_slice::<ErrorEnvelope>(&body) {
        let code = envelope.error.code;
        if let Some((message, retryable)) = bp::friendly_backup_error(&code) {
            return AppError::new(&code, message, retryable);
        }
        return crate::error::from_code(&code);
    }

    tracing::warn!(
        status = status.as_u16(),
        "backup request failed without a code"
    );
    from_code(match status.as_u16() {
        401 | 403 => "BACKUP_CREDENTIAL_INVALID",
        429 => "RATE_LIMITED",
        _ => "UNREACHABLE",
    })
}

/// How the engine should react to a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retry {
    /// Transient. Back off and try the same operation again.
    Backoff,
    /// This entry cannot be sent now, but others can. Skip and revisit later.
    SkipEntry,
    /// Authorization is gone. Stop everything for this profile.
    Fatal,
}

/// Classify an error rather than blindly retrying.
///
/// Retrying a revoked credential just produces a tighter loop of rejections,
/// and retrying a path the server will never accept never succeeds. Only
/// genuinely transient conditions are worth a backoff.
pub fn classify(error: &AppError) -> Retry {
    match error.code.as_str() {
        // Authorization is gone; nothing will work until a person acts.
        "BACKUP_CREDENTIAL_INVALID"
        | "BACKUP_DEVICE_UNPAIRED"
        | "BACKUP_DISABLED"
        | "BACKUP_FORBIDDEN"
        | "BACKUP_NOT_SUPPORTED"
        | "DEVICE_REVOKED" => Retry::Fatal,

        // Wrong about this one entry, and re-sending it unchanged will not help.
        "PATH_TRAVERSAL"
        | "PATH_INVALID"
        | "PATH_TOO_LONG"
        | "BACKUP_IDEMPOTENCY_CONFLICT"
        | "BACKUP_READ_ONLY"
        | "VALIDATION_ERROR"
        // A file bigger than the server accepts will be exactly as big next
        // time. Left to the default it would be treated as a network problem
        // and retried forever, holding up everything queued behind it.
        | "UPLOAD_TOO_LARGE"
        // The server does not know this entry, so a move or a removal aimed at
        // it cannot land. Reconciliation notices it is missing and sends it
        // again; retrying the same doomed request would not.
        | "BACKUP_ENTRY_NOT_FOUND"
        | "BACKUP_FILE_UNREADABLE" => Retry::SkipEntry,

        // Network, server restart, rate limit: exactly what backoff is for.
        _ => Retry::Backoff,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(code: &str) -> AppError {
        crate::error::from_code(code)
    }

    #[test]
    fn revoked_authorization_is_never_retried() {
        // Retrying a revoked grant only produces a tighter loop of rejections.
        for code in [
            "BACKUP_CREDENTIAL_INVALID",
            "BACKUP_DEVICE_UNPAIRED",
            "BACKUP_DISABLED",
            "BACKUP_FORBIDDEN",
            "DEVICE_REVOKED",
        ] {
            assert_eq!(classify(&err(code)), Retry::Fatal, "{code}");
        }
    }

    #[test]
    fn permanent_rejections_skip_the_entry_not_the_backup() {
        // One unacceptable path must not stall everything behind it.
        for code in ["PATH_TRAVERSAL", "PATH_INVALID", "PATH_TOO_LONG"] {
            assert_eq!(classify(&err(code)), Retry::SkipEntry, "{code}");
        }
    }

    #[test]
    fn transient_failures_back_off() {
        for code in ["TIMEOUT", "UNREACHABLE", "RATE_LIMITED", "INTERNAL_ERROR"] {
            assert_eq!(classify(&err(code)), Retry::Backoff, "{code}");
        }
    }

    #[test]
    fn a_locked_file_is_skipped_and_revisited() {
        // Outlook's data file, an open database: pending, not fatal.
        assert_eq!(classify(&err("BACKUP_FILE_UNREADABLE")), Retry::SkipEntry);
    }

    #[test]
    fn a_borrowed_session_cannot_be_printed() {
        let session = crate::connection::webview::BorrowedSession::from_raw("secret-token".into());
        assert_eq!(format!("{session:?}"), "BorrowedSession(<redacted>)");
        assert!(!format!("{session:?}").contains("secret-token"));
    }
}

/// What the Arciin server's disk looks like.
///
/// `GET /api/instance/storage-summary`, which any signed-in role may call. It
/// answers the question a person actually asks before turning on backup —
/// "will it fit?" — which no amount of local file counting can.
///
/// Every field is optional because the server reports `null` when it cannot
/// probe the volume (a network share, an unusual filesystem). A missing number
/// is shown as unknown rather than guessed at.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerStorage {
    #[serde(default)]
    pub usage_bytes: Option<i64>,
    #[serde(default)]
    pub total_bytes: Option<i64>,
    #[serde(default)]
    pub available_bytes: Option<i64>,
}

/// Read the server's storage summary using the signed-in session.
///
/// The user's session rather than the sync credential: this is instance-wide
/// information, not something the backup grant is scoped to.
pub async fn server_storage(
    origin: &Url,
    session_cookie_header: &str,
) -> Result<ServerStorage, AppError> {
    let url = origin
        .join("/api/instance/storage-summary")
        .map_err(|_| from_code("ADDRESS_INVALID"))?;

    let client = build_client(CONTROL_TIMEOUT)?;
    let response = client
        .get(url)
        .header("Cookie", session_cookie_header)
        .send()
        .await?;

    if !response.status().is_success() {
        return Err(from_code("BACKUP_UNAUTHORIZED"));
    }

    #[derive(Deserialize)]
    struct Envelope {
        data: ServerStorage,
    }
    let parsed = json_from_response::<Envelope>(response).await?;
    Ok(parsed.data)
}

/// Turn computer backup back on for a profile that was disabled.
///
/// Authorised by the signed-in user, for the same reason `disable_profile` is:
/// the credential this issues is the one that was revoked, so a grant cannot
/// be the thing that resurrects itself.
///
/// No roots are sent. Disabling a profile marks every folder `DISABLED`
/// server-side, and leaving them that way is the point — turning backup back
/// on must not silently restart uploading folders somebody switched off. Each
/// folder is resumed deliberately, one `enable_root` at a time.
///
/// The server always rotates the credential here, so the returned one replaces
/// whatever this computer held.
pub async fn enable_profile(
    origin: &Url,
    session_cookie_header: &str,
    profile_id: &str,
) -> Result<EnableOutcome, AppError> {
    let url = origin
        .join(&format!("/api/backup/profiles/{profile_id}/enable"))
        .map_err(|_| from_code("ADDRESS_INVALID"))?;

    let client = build_client(CONTROL_TIMEOUT)?;
    let response = client
        .post(url)
        .header("Cookie", session_cookie_header)
        .json(&serde_json::json!({ "roots": [] }))
        .send()
        .await?;

    if !response.status().is_success() {
        let err = backup_error(response).await;
        tracing::warn!(code = %err.code, "re-enabling backup was rejected");
        return Err(err);
    }

    let parsed: Envelope<EnableData> = json_from_response(response).await?;
    tracing::info!(
        profile_id = %parsed.data.profile.id,
        credential_issued = parsed.data.credential_issued,
        "computer backup re-enabled"
    );
    Ok(EnableOutcome {
        profile: parsed.data.profile,
        credential: parsed.data.credential,
    })
}

/// Turn computer backup off for this machine, on the server.
///
/// Authorised by the signed-in user rather than the sync credential: the
/// credential is exactly what this revokes, and a grant must not be able to
/// destroy its own authority.
///
/// What the server does — and what it deliberately does not: the profile is
/// disabled and its grants revoked, so the credential this computer holds
/// stops working immediately. The Device stays paired, the user stays signed
/// in, and every file already stored stays where it is. Stopping backup is not
/// disconnecting the computer, and it is not a delete.
pub async fn disable_profile(
    origin: &Url,
    session_cookie_header: &str,
    profile_id: &str,
) -> Result<(), AppError> {
    let url = origin
        .join(&format!("/api/backup/profiles/{profile_id}/disable"))
        .map_err(|_| from_code("ADDRESS_INVALID"))?;

    let client = build_client(CONTROL_TIMEOUT)?;
    let response = client
        .post(url)
        .header("Cookie", session_cookie_header)
        .send()
        .await?;

    if !response.status().is_success() {
        return Err(backup_error(response).await);
    }
    Ok(())
}
