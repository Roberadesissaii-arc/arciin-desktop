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
