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
