/**
 * Display helpers shared by the onboarding screens.
 *
 * These mirror the equivalents in `src-tauri/src/protocol.rs` and
 * `src-tauri/src/address.rs`. The native side remains the authority - it is
 * what talks to the server - but the same rules are needed here to render a
 * code or an address before a round trip, so both are kept in step and tested.
 */

const CODE_LENGTH = 6

/** Strip everything but digits and clamp to the code length. */
export function normalizeCode(input: string): string {
  return input.replace(/\D/g, "").slice(0, CODE_LENGTH)
}

/** `482731` -> `482 731`, the grouping the server's Settings page shows. */
export function formatCode(input: string): string {
  const digits = normalizeCode(input)
  if (digits.length <= 3) return digits
  return `${digits.slice(0, 3)} ${digits.slice(3)}`
}

export function isCompleteCode(input: string): boolean {
  return normalizeCode(input).length === CODE_LENGTH
}

/**
 * How a saved server's address reads on a card.
 *
 * Plain HTTP on a default port is the common LAN case and its scheme is noise,
 * so it is dropped. A port or a TLS scheme carries information, so it stays.
 */
export function hostLabel(baseUrl: string): string {
  let url: URL
  try {
    url = new URL(baseUrl)
  } catch {
    return baseUrl
  }
  const port = url.port ? `:${url.port}` : ""
  if (url.protocol === "http:") return `${url.hostname}${port}`
  return `${url.protocol}//${url.hostname}${port}`
}

/** An IPv4 literal, captured so the final octet can be replaced. */
const IPV4 = /^(\d{1,3}\.\d{1,3}\.\d{1,3})\.(\d{1,3})$/

/**
 * Hide the host part of a LAN address while keeping it recognisable.
 *
 * `192.168.1.50:3002` -> `192.168.1.xxx:3002`
 *
 * The subnet and port stay, because those are what a person uses to recognise
 * which network and which service this is. The final octet is the part that
 * identifies one specific machine, and it is not needed to tell saved servers
 * apart — the instance name does that. Nothing is masked for a hostname, which
 * carries no per-machine address to hide.
 */
export function maskAddress(label: string): string {
  const [host, port] = splitPort(label)
  const match = IPV4.exec(host)
  if (!match) return label
  return port ? `${match[1]}.xxx:${port}` : `${match[1]}.xxx`
}

/** Split `host:port`, leaving bracketed IPv6 and bare hosts untouched. */
function splitPort(label: string): [string, string | null] {
  const index = label.lastIndexOf(":")
  if (index === -1 || label.includes("]")) return [label, null]
  const port = label.slice(index + 1)
  if (!/^\d+$/.test(port)) return [label, null]
  return [label.slice(0, index), port]
}

/**
 * Human file sizes, in the units Windows itself shows.
 *
 * Binary units (1024) because that is what Explorer reports; a "GB" that
 * disagrees with the Properties dialog reads as a bug.
 */
export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return "0 B"
  const units = ["B", "KB", "MB", "GB", "TB"]
  const exponent = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1)
  const value = bytes / 1024 ** exponent
  // One decimal once past bytes and KB, where a fraction is meaningless.
  const decimals = exponent <= 1 ? 0 : value >= 100 ? 0 : 1
  return `${value.toFixed(decimals)} ${units[exponent]}`
}

/** Thousands separators, so 12430 reads as 12,430. */
export function formatCount(value: number): string {
  if (!Number.isFinite(value) || value < 0) return "0"
  return Math.round(value).toLocaleString("en-US")
}

/**
 * What a folder that is no longer being backed up actually has, in one phrase.
 *
 * Two independent facts, and all four combinations happen:
 *
 *   folder here, files stored     "4 files · 23 B on your server"
 *   folder here, nothing stored   "Nothing backed up yet"
 *   folder gone, files stored     "… on your server · Local folder not found"
 *   folder gone, nothing stored   "Nothing backed up yet · Local folder not found"
 *
 * Neither may be assumed from the other. A folder can be dropped before a
 * single file uploads, so "still stored on your Arciin server" as blanket copy
 * would be false — and this list is exactly where somebody checks. A folder can
 * equally be deleted from this PC long after its files were safely stored, and
 * pretending it is still here would send them looking for it.
 */
export function describeDormantFolder(root: {
  fileCount: number
  bytesSynced: number
  localPathExists: boolean
}): string {
  const stored =
    root.fileCount > 0
      ? `${formatCount(root.fileCount)} files · ${formatBytes(root.bytesSynced)} on your server`
      : "Nothing backed up yet"
  return root.localPathExists ? stored : `${stored} · Local folder not found`
}
