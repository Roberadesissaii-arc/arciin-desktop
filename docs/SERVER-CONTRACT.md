# Server contract

What this client assumes about an Arciin server, and where those assumptions
come from. This file does **not** restate the protocol — the specification
lives with the server and is the single source of truth.

## Authority

| | |
| --- | --- |
| Protocol specification | `docs/DESKTOP-PAIRING-PROTOCOL.md` in the Arciin server repository |
| Reference source tree | `<path-to>\arciin-main` (read-only) |
| Server feature branch | `feature/device-pairing` |
| Server feature commit | `1321e074a0efab138b9df97621f7942bc666d8fa` |
| Certified baseline commit | `9a3199d1ec11ad5c00ac2f108e84178fbe34c20c` |
| **Supported device protocol** | **1** |

Nothing in this client may invent an endpoint, a payload field or an error
code. Every one is mirrored from the specification and cross-checked against
the server implementation:

- `apps/api/src/modules/devices/routes.ts`
- `apps/api/src/services/devices/pairing.ts`
- `apps/api/src/services/devices/discovery.ts`
- `apps/api/src/services/devices/device-cookie.ts`
- `packages/config/src/device-pairing.ts`

The client-side copy of these constants is `src-tauri/src/protocol.rs`.

## Endpoints this client uses

All four are reached on the **web origin** (for example
`http://203.0.113.10:3002`), which proxies `/api/*` to the Fastify API and
serves `/.well-known/arciin` from it. The web origin is the right base because
it is also what the WebView loads, so the trusted-device cookie lands on the
same origin as the application.

| Method | Path | Auth | Used for |
| --- | --- | --- | --- |
| `GET` | `/.well-known/arciin` | none | Discovery, identity verification |
| `POST` | `/api/devices/pair` | none (rate-limited) | One-time pairing claim |
| `POST` | `/api/devices/session` | `Authorization: Device <credential>` | Device bootstrap |
| — | *(the web application itself)* | trusted-device cookie + user session | The product UI |

Device management — listing, generating a PIN, renaming, revoking — is
deliberately **not** implemented here. Those endpoints require a signed-in
OWNER/ADMIN session and belong to the server's own Settings → Devices page.

## Backup lifecycle

Backup is **server-authoritative**. The local SQLite database is a cache of the
upload queue; it is not the authority on whether this computer is allowed to
back up, because that can be changed from another device while this app is
closed.

Two authorities, deliberately separate:

| Method | Path | Auth | Used for |
| --- | --- | --- | --- |
| `POST` | `/api/backup/profiles` | user session | Enable backup, create/upsert roots |
| `POST` | `/api/backup/profiles/:id/disable` | user session | Turn backup off |
| `POST` | `/api/backup/profiles/:id/enable` | user session | Turn backup back on |
| `GET` | `/api/backup/me` | `ArciinSync` grant | Reconcile on launch |
| `POST` | `/api/backup/roots/:id/disable` | grant *or* session | Stop protecting a folder |
| `POST` | `/api/backup/roots/:id/enable` | grant *or* session | Protect it again |

The profile-level calls take the signed-in user's session rather than the sync
grant, and that is the point: the grant is exactly what disabling revokes and
what enabling issues, so a credential must not be able to revoke or resurrect
its own authority.

What the server does on each, which the client depends on:

- **Disable** revokes every grant for the profile and marks every `SyncRoot`
  `DISABLED`. The `Device` stays paired, the user stays signed in, and every
  file already stored stays where it is.
- **Enable** reuses the existing profile — same id, same device folder — and
  always issues a fresh credential because the profile was disabled. Roots sent
  in the body are upserted by `sourcePathIdentifier` and become `PROTECTED`;
  roots left out stay `DISABLED`. The client deliberately sends none, so
  turning backup back on never restarts uploading a folder somebody switched
  off.
- **Root enable** refuses with `BACKUP_DISABLED` while the profile is off, so
  the two can never be re-enabled in the wrong order.

Because roots are upserted by `sourcePathIdentifier` and re-enabled by id, a
folder that is removed and added again is the *same* `SyncRoot`. Nothing is
duplicated in the computer's hierarchy, and the files already stored under it
are recognised rather than re-sent.

### Reconciliation on launch

Before resuming, the client asks `GET /api/backup/me` with the grant it holds:

- `BACKUP_DISABLED`, `BACKUP_CREDENTIAL_INVALID` or `BACKUP_NOT_FOUND` — backup
  is off. The credential is dropped and the local queue cleared, but the
  profile and folder list are **kept**, because they are what the offer to turn
  backup back on is made of.
- A trust-lost code — a different event entirely, handled by the device
  watchdog, not here.
- Any other failure, including no answer at all — unknown, not "no". Being
  unable to ask is not being told no, and tearing down a working setup because
  the Wi-Fi is down would be its own bug.
- Success — each local root is checked against the server's, and any the server
  no longer protects stops being scanned here.

## Protocol version handling

`ARCIIN_DEVICE_PROTOCOL_VERSION = 1`.

The client sends `protocolVersion: 1` and refuses any manifest whose
`protocolVersion` is not exactly 1, surfacing `DEVICE_PROTOCOL_UNSUPPORTED`.
Verified against the live server: sending `protocolVersion: 2` returns
`400 DEVICE_PROTOCOL_UNSUPPORTED`.

## Error codes consumed

Every code in section 11 of the specification is mapped to its own message in
`src-tauri/src/error.rs`, and a test asserts none of them falls through to the
generic fallback. `PAIRING_REQUIRED` is mapped but, per the specification, is
not returned in V1.

## What the client requires from the server

1. `GET /.well-known/arciin` reachable **unauthenticated on the web origin**.
2. A stable UUID `serverId` that does not change across restarts.
3. `POST /api/devices/pair` returning `data.credential` exactly once.
4. `POST /api/devices/session` responding with a `Set-Cookie` header for
   `arciin_trusted_device`. **This header is what makes the WebView bridge
   possible** — see `docs/SECURITY-ARCHITECTURE.md`.
5. The web origin's `/api/*` proxy must forward both the `Authorization`
   request header and the `Set-Cookie` response header. The reference
   implementation does (`apps/web/lib/server/api-proxy.ts` copies every
   non-hop-by-hop header in each direction).

## Known server-side observations

- **mDNS is not advertised.** The live manifest reports
  `mdns.advertised: false`, and the server documents Docker bridge networking
  as unable to carry multicast reliably. Automatic discovery therefore finds
  nothing on a standard install, and the manual address path is the primary
  route. The client treats this as normal, not as an error.
- **`webUrl` in the manifest can be stale or unreachable.** The live server
  reports `webUrl: http://203.0.113.11:3002` while actually serving on
  `203.0.113.10:3002`. The client therefore connects to the origin it
  successfully fetched the manifest from and uses `webUrl` for nothing.

## What the watcher sends

Nothing new. The watcher decides *when* and *which* of the existing calls are
made; it introduces no endpoint, no field and no header.

| Local event | Call | Notes |
| --- | --- | --- |
| A file appears or changes | `POST /api/backup/files` | The bytes are read when the request is made, not when the event arrived |
| A folder appears | `POST /api/backup/folders` | Parents first, always |
| Something is renamed or moved within one protected folder | `POST /api/backup/entries/:id/move` | Same `clientEntryId`, so the server keeps its identity and its history |
| Something is deleted | `POST /api/backup/entries/:id/tombstone` | Soft delete. This client never destroys anything on the server |

Two shapes the client deliberately does **not** send:

- **A move between two protected folders.** Each root has its own opaque
  identity, and one entry cannot belong to both. It is a tombstone in the
  folder it left and an upload in the one it arrived in.
- **Anything at all when a folder cannot be read.** A disconnected drive
  presents exactly as "every file was deleted". The client reports the folder
  unavailable and sends nothing, because the alternative is tombstoning
  somebody's backup because a cable came loose.

Absolute Windows paths remain local under all of this. The watcher deals in
them by necessity; what leaves the machine is still a root identity and a
relative path.
