# Arciin Desktop

A native Windows client that connects a computer to a **private Arciin server**
you run yourself. It finds the server on your network, pairs this computer as a
trusted device, and opens the Arciin interface that server hosts.

There is no Arciin cloud in the middle. The server is yours, and this client
talks only to the one you paired it with.

> **Status: pre-release (0.1.x).** Not yet published for public download. See
> [Current status](#current-status) for what is finished and what is not.

---

## What it does

**Finds your server.** Browses the local network over mDNS
(`_arciin._tcp.local.`), or takes an address you type. Every candidate is
verified against the server's discovery manifest before anything is sent to it.

**Pairs once.** You generate a pairing code in Arciin's own
Settings → Devices, enter it here, and this computer becomes a trusted device.
The credential the server issues is stored in **Windows Credential Manager**,
namespaced per server. It never touches disk in plain text, never reaches the
UI layer, and there is no command that returns it.

**Opens the real Arciin.** The product UI is served by your server and rendered
in WebView2. This client deliberately contains no copy of Dashboard, Files or
Settings — it is a shell, not a reimplementation. The trusted-device session is
handed to the webview as an `HttpOnly` cookie, exactly as a browser would have
received it.

## Current status

| Area | State |
| --- | --- |
| Server discovery (mDNS + manual) | Working |
| Pairing and trusted-device credential | Working |
| Credential storage (Windows Credential Manager) | Working |
| Native WebView shell | Working |
| Windows installer (NSIS) | Working |
| **Authenticode code signing** | **Not configured** |

**Nothing runs in the background.** This client is a shell around your
server's own interface. It holds no copy of your files, and while it is
closed it does nothing at all.

## Security model

The short version, in full in [`docs/SECURITY-ARCHITECTURE.md`](docs/SECURITY-ARCHITECTURE.md):

- **Two separate credentials**, never mixed: the trusted-device credential
  and your signed-in user session.
- **Secrets live in Windows Credential Manager**, not in files and not in the
  renderer.
- **The server's page gets no IPC.** The webview rendering your server's UI is
  in no Tauri capability, so it cannot invoke a single native command.
- **Nothing sensitive is logged.** No credential, cookie, `Authorization`
  header, password or file content is ever passed to a logging macro.

## Building from source

Requires Windows 10/11 x64, [Rust](https://rustup.rs), Node.js 20+, and the
[WebView2 runtime](https://developer.microsoft.com/microsoft-edge/webview2/)
(already present on current Windows).

```bash
npm install
npm run tauri:build
```

The installer lands in
`src-tauri/target/release/bundle/nsis/`.

### Development

```bash
npm run tauri:dev     # run with hot reload
npm test              # frontend tests
npm run typecheck     # TypeScript
cd src-tauri && cargo test && cargo clippy --all-targets && cargo fmt --check
```

### Installing a local build

```bash
powershell -File scripts/install.ps1
```

This closes a running instance **gracefully** before installing. That matters:
force-killing the process skips WebView2's cookie flush, which silently
discards your signed-in session. Use `scripts/stop.ps1` to stop the app for the
same reason.

## Versioning

`package.json` is the single source of truth. `tauri.conf.json` reads it,
Vite injects it into the UI, and `node scripts/check-version.mjs` fails the
build if `Cargo.toml` has drifted.

- **0.1.x** — pre-public development. Interfaces may change.
- **1.0.0** — only once the release criteria in
  [`docs/RELEASE.md`](docs/RELEASE.md) are met.

## Repository layout

```
src/              React UI for the onboarding screens
src-tauri/        Rust: discovery, pairing, credentials
  src/connection/ WebView shell, navigation guard, trust watchdog
scripts/          Icon generation, install/stop helpers, version check
docs/             Server contract, security architecture, release process
tests/            Frontend tests (Rust tests live beside their modules)
```

## License

Not yet chosen. Until a license is added, all rights are reserved.
