# Releasing Arciin Desktop

How a build becomes a public Windows download, and what has to be true first.

**Nothing here is published yet.** The pipeline exists; the gate below is not
met.

---

## Versioning

`package.json` is the single source of truth.

| File | How it gets the version |
| --- | --- |
| `package.json` | **The version.** Edit here, nowhere else. |
| `src-tauri/tauri.conf.json` | `"version": "../package.json"` — Tauri reads it |
| `src/app/App.tsx` (footer) | `__APP_VERSION__`, injected by Vite |
| `src-tauri/Cargo.toml` | Must be edited to match; CI fails if it drifts |

`node scripts/check-version.mjs` enforces this and runs in both workflows. It
exists because the footer used to be a hard-coded string: an installer and an
app reporting different versions is the kind of thing nobody notices until a
bug is filed against a version that never shipped.

### Policy

- **0.1.x** — pre-public development. Interfaces may change without notice.
- **1.0.0** — only when every box in [the gate](#public-release-gate) is ticked.

The current build is **not** 1.0 and must not be described as one.

## Building a release

```bash
npm ci
npm run tauri:build
node scripts/release-metadata.mjs
```

Produces `src-tauri/target/release/bundle/nsis/*-setup.exe` and
`release/desktop-windows.json`.

The build is reproducible from a clean checkout: no absolute paths, no
pre-existing `target/` cache, no local secret files, no manually copied DLLs.
The only host requirements are Rust, Node 20+, and the Windows SDK that
`tauri-winres` uses for the resource compiler.

> **Known wart.** Tauri derives the installer filename from `productName`, so
> it currently contains a space: `Arciin Desktop_0.1.0_x64-setup.exe`. That is
> awkward in a URL. Either rename the asset when publishing, or decide to
> change `productName` — which also changes the installed application name, so
> it is a product decision, not a build tweak.

## Workflows

| Workflow | Trigger | Produces |
| --- | --- | --- |
| `ci.yml` | every push to `main`, every PR | nothing publishable |
| `release.yml` | tag `desktop-v*`, or manual dispatch | **draft** release + artifacts |

`release.yml` never runs on a push to `main`. Shipping is a decision, not a
side effect of merging.

Every release build re-runs the full CI gate first. A tag must not be able to
ship something that would have failed review.

### Why the release is a draft

A draft release is invisible to the public and its assets are not
downloadable. That is the safety property: an installer cannot silently become
the website's download. Publishing is a separate, human action taken only once
the gate is met.

## Code signing

**CODE SIGNING: NOT CONFIGURED.**

There is no Authenticode certificate. Unsigned installers trigger SmartScreen
warnings, so this is a hard requirement before public distribution — but it
does not block publishing *source*.

No self-signed certificate has been generated. A self-signed build is not
production signing and calling it so would be worse than leaving it unsigned.

### Enabling it later

The interface is already in `release.yml`. Add two repository secrets:

| Secret | Value |
| --- | --- |
| `WINDOWS_CERTIFICATE` | the `.pfx`, base64-encoded |
| `WINDOWS_CERTIFICATE_PASSWORD` | its password |

The signing step is skipped entirely while `WINDOWS_CERTIFICATE` is absent, so
the pipeline works today without pretending.

**Never commit** a `.pfx`, `.p12`, private key or password. `.gitignore`
blocks those patterns, but the rule matters more than the file.

What a certificate needs to be: an OV or EV code-signing certificate from a
CA Windows trusts. EV gets SmartScreen reputation immediately; OV builds it
over time. An HSM-backed or cloud signing service (Azure Trusted Signing,
DigiCert KeyLocker) is preferable to a `.pfx` in CI secrets, and the step can
be swapped for one without touching the rest of the pipeline.

### Verification

`release.yml` runs `Get-AuthenticodeSignature` after building and records the
result in the metadata's `signed` field. An unsigned build is reported as an
**unsigned development artifact** and `signed: false`. The website must refuse
to offer anything that is not `signed: true`.

## Release metadata

`scripts/release-metadata.mjs` writes `release/desktop-windows.json`:

```json
{
  "product": "arciin-desktop",
  "version": "0.1.0",
  "platform": "windows",
  "arch": "x86_64",
  "installer": {
    "filename": "...-setup.exe",
    "url": "",
    "sizeBytes": 0,
    "sha256": "..."
  },
  "signed": false,
  "minimumOs": "Windows 10 (1809) x64",
  "requires": ["Microsoft Edge WebView2 Runtime"],
  "publishedAt": "..."
}
```

`url` is intentionally empty. It is only knowable once the release asset
exists, and a manifest that guesses points at something that is not there.
Whoever publishes fills it in.

Installers are **never committed**. They are release assets, hosted on GitHub
Releases.

## Updates

**Not implemented.** No updater plugin is installed, and automatic updating is
off. This section is the intended architecture, not a description of code that
exists.

The shape, when it is built:

```
installed app
  → fetch signed release metadata over HTTPS
  → compare version with its own
  → verify signature and SHA-256 before trusting anything
  → tell the user; never install silently
  → download, verify again, hand to the installer
  → installer closes the app gracefully, replaces it, restarts
```

Three things must be true before it is switched on:

1. **Signing works**, so an update can be verified as genuinely ours. An
   unsigned auto-update is a remote code execution channel.
2. **Hosting is stable**, so a URL in a shipped binary keeps resolving.
3. **Rollback is understood**: what happens when an update fails halfway, and
   how a user recovers a working install.

Until then the update path is: download the new installer and run it.

### Graceful shutdown is part of this

Any update mechanism must close the app the way `scripts/stop.ps1` does.
WebView2 flushes its cookie store on shutdown; killing the process skips that
and silently discards the user's signed-in session. An updater that force-kills
would sign people out on every update — which is exactly the bug that
`install.ps1` was changed to avoid.

## Public release gate

Public distribution is **BLOCKED** until every box is ticked.

- [x] Continuous filesystem watcher complete
- [x] Reconciliation after offline changes complete
- [ ] Watcher certified on a second machine and over a long run
- [x] Root disable/reactivate lifecycle integrated server-side
- [x] Profile stop/re-enable integrated — the server disables the profile and
      revokes the grant, the client keeps the profile and folder list so it can
      offer them back, and re-enabling reuses the same profile with a freshly
      rotated credential
- [ ] Stop → re-enable certified against a live server (grant rotation proved,
      no duplicate hierarchy)
- [ ] Backup Center certified
- [ ] Pairing and revocation certified
- [ ] Graceful shutdown certified
- [ ] "Remember me" session persistence certified
- [ ] Clean install tested on a machine that never had Arciin
- [ ] Upgrade from a previous version tested
- [ ] NSIS artifact reproducible from a clean checkout
- [ ] Secret audit clean
- [ ] Authenticode signing configured
- [ ] Installer signature verified in the pipeline
- [ ] Release asset hosted
- [ ] Release metadata generated with a real `url` and `signed: true`
- [ ] Website manifest updated
- [ ] Smoke test on a clean Windows machine

Source is public before this gate; the installer is not. Those are separate
decisions and should stay that way.

## Certifying the backup lifecycle

The one claim about backup that no local test can establish is that stopping it
**revokes** the credential this computer holds, and that turning it back on
issues a different one. Only a server that hashes, stores and revokes grants can
show that, and only if it is the real one.

`src-tauri/tests/lifecycle_live.rs` drives the real client functions over real
HTTP against a real Arciin API. It skips unless `ARCIIN_CERT_ORIGIN` is set, so
CI and ordinary `cargo test` runs are unaffected.

**Point it only at a disposable instance.** It disables and re-enables the
profile it is given. Against a live instance it would stop somebody's backup.

Setting one up — a scratch database and a throwaway device, nothing shared with
a running instance:

1. A PostgreSQL cluster that is not the instance's own, and an empty database.
2. A checkout of the server **copied** somewhere scratch, so the real one is
   never written to, with `DATABASE_URL` pointed at that database and
   `prisma db push` run against it.
3. The API booted on a spare port, seeded with a user, a paired device, a
   backup profile and one root, printing the origin, profile id, credential and
   session cookie.

Then:

```bash
ARCIIN_CERT_ORIGIN=http://127.0.0.1:4310 \
ARCIIN_CERT_PROFILE_ID=... \
ARCIIN_CERT_CREDENTIAL_A=... \
ARCIIN_CERT_SESSION_COOKIE=... \
  cargo test --test lifecycle_live -- --nocapture
```

It asserts the sequence end to end: A accepted → profile disabled → A rejected
→ profile re-enabled → same profile and device, fresh credential B → A *still*
rejected → B accepted → no root, profile or device duplicated.

What it cannot assert from the client side, and what the server's own
integration suite covers instead: that the stored grant is a hash rather than
the credential. Run `pnpm test:integration` on the scratch copy for that.

`src-tauri/tests/secret_safety.rs` covers the rest, and needs no server: the
credential reaches exactly one `Authorization` header, never a URL, never a
logging macro, never the local database, and never the state handed to the UI.
