/**
 * Fail the build if the declared versions have drifted apart.
 *
 * `package.json` is the source of truth. `tauri.conf.json` points at it and
 * `vite.config.ts` injects it, so neither can disagree. `Cargo.toml` is the
 * one that cannot reference another file, so it is checked here instead of
 * being trusted — a mismatch means the installer and the app would report
 * different versions, which is the kind of thing nobody notices until a user
 * reports a bug against a version that was never shipped.
 *
 *   node scripts/check-version.mjs
 */

import { readFileSync } from "node:fs"
import { fileURLToPath, URL } from "node:url"

const read = (relative) =>
  readFileSync(fileURLToPath(new URL(relative, import.meta.url)), "utf-8")

const pkg = JSON.parse(read("../package.json"))
const cargo = read("../src-tauri/Cargo.toml")
const tauri = JSON.parse(read("../src-tauri/tauri.conf.json"))

const cargoVersion = cargo.match(/^version\s*=\s*"([^"]+)"/m)?.[1]

const problems = []

if (!/^\d+\.\d+\.\d+$/.test(pkg.version)) {
  problems.push(`package.json version "${pkg.version}" is not SemVer x.y.z`)
}

if (cargoVersion !== pkg.version) {
  problems.push(
    `src-tauri/Cargo.toml is ${cargoVersion}, package.json is ${pkg.version}`,
  )
}

// Tauri must defer rather than carry its own copy.
if (tauri.version !== "../package.json") {
  problems.push(
    `src-tauri/tauri.conf.json should read "../package.json", found "${tauri.version}"`,
  )
}

if (problems.length > 0) {
  console.error("Version drift:")
  for (const problem of problems) console.error(`  - ${problem}`)
  process.exit(1)
}

console.log(`Version ${pkg.version} is consistent across package.json, Cargo.toml and tauri.conf.json.`)
