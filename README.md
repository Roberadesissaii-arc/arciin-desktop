# Arciin Desktop

A native Windows client that connects a computer to a **private Arciin server**
you run yourself. It finds the server on your network, pairs this computer as a
trusted device, opens the Arciin interface that server hosts, and backs up
folders you choose from this PC into it.

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

**Keeps up with changes.** While Arciin Desktop is running, protected folders
are watched: a saved file is uploaded within seconds, a renamed one is moved
rather than uploaded again, and a deleted one is moved to your server's Trash
rather than destroyed. If an unusual number of files disappear at once — the
shape of an unplugged drive, not a decision — backup pauses for that folder and
asks, rather than removing the copies on your server.

**Backs up folders.** Choose Windows known folders (Desktop, Documents,
Pictures, Videos, Music, Downloads) or any folder via the native picker.
Uploads are one-way — this PC is the source, your server is the destination —
and the folder tree is preserved as-is. Nothing is ever deleted from this
computer.

**Manages backup.** A native Backup Center shows what is protected, where each
folder lives on this PC, how much has been stored, and your server's remaining
disk space. Add folders, stop protecting one, pause, resume, or stop backup
entirely — without disconnecting the device.

**Stopping is not a one-way door.** Turning backup off disables the profile on
your server and revokes this computer's backup credential, and the client
remembers which folders were protected so it can offer them back. Turning
backup on again reuses the same profile with a freshly issued credential, and
resuming a folder reactivates the folder that was already there rather than
building a second copy of it beside the first.

## Current status

| Area | State |
| --- | --- |
| Server discovery (mDNS + manual) | Working |
| Pairing and trusted-device credential | Working |
| Credential storage (Windows Credential Manager) | Working |
| Native WebView shell | Working |
| Computer Backup — initial backup | Working |
| Backup Center — add, remove, pause, resume, stop | Working |
| Stop and re-enable backup (server-authoritative) | Working |
| Windows installer (NSIS) | Working |
| Continuous filesystem watcher (while the app is running) | Working |
| Reconciliation after offline changes | Working |
| **Authenticode code signing** | **Not configured** |

**Arciin Desktop watches protected folders while it is running.** Create,
edit, rename, move or delete a file in a protected folder and the change
reaches your server within seconds — no restart, no "sync now".

**It is not a background service.** While Arciin Desktop is closed nothing is
watching, and nothing is uploaded. Changes made in the meantime are found by
the reconciliation scan the next time it starts, so nothing is lost — but
backup only runs while the app does. A Windows service is not part of this
version.

Changes are also reconciled periodically while it runs, because filesystem
notifications can be dropped. The watcher is what makes it feel immediate; the
scan is what makes it correct.

## Security model

The short version, in full in [`docs/SECURITY-ARCHITECTURE.md`](docs/SECURITY-ARCHITECTURE.md):

- **Three separate credentials**, never mixed: the device credential, the
  computer-backup (`ArciinSync`) credential, and your signed-in user session.
- **Secrets live in Windows Credential Manager**, not in files, not in the
  renderer, not in the local database.
- **The server's page gets no IPC.** The webview rendering your server's UI is
  in no Tauri capability, so it cannot invoke a single native command. The one
  thing it may ask for — open the native backup setup screen — arrives as a
  navigation to a fixed sentinel URL that carries no path, argument or command
  name.
- **Absolute local paths stay local.** The server receives an opaque, salted
  identifier per protected folder, never `C:\Users\<user>\...`.
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
src/              React UI for the onboarding and backup surfaces
src-tauri/        Rust: discovery, pairing, credentials, backup engine
  src/backup/     Scanning, queue, upload engine, local sync database
  src/connection/ WebView shell, navigation guard, trust watchdog
scripts/          Icon generation, install/stop helpers, version check
docs/             Server contract, security architecture, release process
tests/            Frontend tests (Rust tests live beside their modules)
```

## License

Not yet chosen. Until a license is added, all rights are reserved.
