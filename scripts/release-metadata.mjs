/**
 * Describe a built installer, for the website's download manifest.
 *
 * Writes `release/desktop-windows.json`. Nothing here is hard-coded to a
 * machine: the version comes from `package.json`, the installer is found by
 * pattern, and the size and digest are measured from the file itself.
 *
 * The `signed` flag is honest. An unsigned development build says so, and the
 * website is expected to refuse anything that is not `signed: true` — an
 * unsigned installer must never become a public download by omission.
 *
 * The URL is left empty deliberately. It is only knowable once a release is
 * published, and guessing it would let a manifest point at an asset that does
 * not exist.
 *
 *   node scripts/release-metadata.mjs
 */

import { createHash } from "node:crypto"
import { mkdirSync, readFileSync, readdirSync, statSync, writeFileSync } from "node:fs"
import { join } from "node:path"
import { fileURLToPath, URL } from "node:url"

const root = fileURLToPath(new URL("..", import.meta.url))
const pkg = JSON.parse(readFileSync(join(root, "package.json"), "utf-8"))

const bundleDir = join(root, "src-tauri", "target", "release", "bundle", "nsis")

let installer
try {
  installer = readdirSync(bundleDir).find((name) => name.endsWith("-setup.exe"))
} catch {
  console.error(`No bundle directory at ${bundleDir}. Run: npm run tauri:build`)
  process.exit(1)
}
if (!installer) {
  console.error(`No *-setup.exe in ${bundleDir}. Run: npm run tauri:build`)
  process.exit(1)
}

const installerPath = join(bundleDir, installer)
const bytes = readFileSync(installerPath)

const metadata = {
  product: "arciin-desktop",
  version: pkg.version,
  platform: "windows",
  arch: "x86_64",
  installer: {
    filename: installer,
    // Filled in by whoever publishes the release asset. Empty means "not
    // published yet", which the website must treat as "no download".
    url: "",
    sizeBytes: statSync(installerPath).size,
    sha256: createHash("sha256").update(bytes).digest("hex"),
  },
  // Authenticode. `false` until a certificate is configured; see docs/RELEASE.md.
  signed: process.env.ARCIIN_SIGNED === "true",
  minimumOs: "Windows 10 (1809) x64",
  requires: ["Microsoft Edge WebView2 Runtime"],
  publishedAt: new Date().toISOString(),
}

const outDir = join(root, "release")
mkdirSync(outDir, { recursive: true })
const outPath = join(outDir, "desktop-windows.json")
writeFileSync(outPath, `${JSON.stringify(metadata, null, 2)}\n`)

console.log(`Wrote ${outPath}`)
console.log(`  version  ${metadata.version}`)
console.log(`  file     ${metadata.installer.filename}`)
console.log(`  size     ${metadata.installer.sizeBytes} bytes`)
console.log(`  sha256   ${metadata.installer.sha256}`)
console.log(`  signed   ${metadata.signed}`)
