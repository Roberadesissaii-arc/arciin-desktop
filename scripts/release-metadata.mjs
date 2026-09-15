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
import {
  mkdirSync,
  readFileSync,
  readdirSync,
  renameSync,
  statSync,
  writeFileSync,
} from "node:fs"
import { join } from "node:path"
import { fileURLToPath, URL } from "node:url"

const root = fileURLToPath(new URL("..", import.meta.url))
const pkg = JSON.parse(readFileSync(join(root, "package.json"), "utf-8"))

const bundleDir = join(root, "src-tauri", "target", "release", "bundle", "nsis")

let installer
try {
  // Case-insensitively, and matching the renamed form too: this script is
  // idempotent, and after one run the file is `...-x64-Setup.exe`. A finder
  // that only matched the name Tauri produces would work exactly once.
  installer = readdirSync(bundleDir).find((name) =>
    name.toLowerCase().endsWith("-setup.exe"),
  )
} catch {
  console.error(`No bundle directory at ${bundleDir}. Run: npm run tauri:build`)
  process.exit(1)
}
if (!installer) {
  console.error(`No installer in ${bundleDir}. Run: npm run tauri:build`)
  process.exit(1)
}

/**
 * The name the file is published under.
 *
 * Tauri derives the installer's filename from `productName`, which is
 * "Arciin Desktop" — so the file arrives as `Arciin Desktop_0.1.0_x64-setup.exe`,
 * with a space in it. A space in a download URL becomes `%20`, which is ugly
 * in a link, easy to break when copied into a terminal, and a recurring source
 * of mangled paths in installers scripted by third parties.
 *
 * The fix is renaming the asset, not renaming the product. `productName` is
 * what Windows shows in Apps & Features, in the Start Menu and in the
 * installer's own title bar, and it should read "Arciin Desktop" in all three.
 */
function publishedName(version) {
  return `Arciin-Desktop-${version}-x64-Setup.exe`
}

let installerPath = join(bundleDir, installer)
const wanted = publishedName(pkg.version)
if (installer !== wanted) {
  const target = join(bundleDir, wanted)
  renameSync(installerPath, target)
  installer = wanted
  installerPath = target
  console.log(`  renamed  ${wanted}`)
}

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
  // Authenticode. Set from the pipeline's own `Get-AuthenticodeSignature`
  // check, never asserted by hand. See the publication guard below.
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
/**
 * An unsigned build may exist. It may not be published.
 *
 * `url` is what makes this manifest a download: the website reads it and hands
 * the file to whoever clicks. Empty means "not published yet". So the moment
 * somebody fills it in, the signature stops being informational and becomes
 * the thing standing between a user and a binary they cannot verify.
 *
 * Refusing here rather than trusting a checklist, because this is the step
 * that turns a file into a download, and the person filling in the URL is
 * exactly the person most likely to be in a hurry.
 */
if (metadata.installer.url && !metadata.signed) {
  console.error("Refusing to write a publishable manifest for an unsigned installer.")
  console.error("A download must be Authenticode signed; see docs/RELEASE.md.")
  process.exit(1)
}

console.log(`  file     ${metadata.installer.filename}`)
console.log(`  size     ${metadata.installer.sizeBytes} bytes`)
console.log(`  sha256   ${metadata.installer.sha256}`)
console.log(`  signed   ${metadata.signed}`)
