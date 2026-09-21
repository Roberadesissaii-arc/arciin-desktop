/**
 * Shapes crossing the Tauri IPC boundary.
 *
 * These mirror the `#[derive(Serialize)]` types in `src-tauri`. Note what has
 * no representation here: the device credential. There is no field for it on
 * any of these types, because it never leaves Rust.
 */

/** A server that answered with a manifest the native layer accepted. */
export type VerifiedServer = {
  serverId: string
  name: string
  /** Canonical origin, e.g. `http://192.168.1.50`. */
  baseUrl: string
  /** What to show on a card, e.g. `192.168.1.50`. */
  displayAddress: string
  protocolVersion: number
  pairingSupported: boolean
  pairingAvailable: boolean
  version?: string
}

/** A server this computer has already paired with. */
export type SavedServer = {
  serverId: string
  name: string
  baseUrl: string
  protocolVersion: number
  lastConnectedAt: string | null
  /** The server told us this device is no longer trusted. */
  revoked: boolean
}

/**
 * The result of pairing. `paired: true` - and deliberately nothing that could
 * carry a secret.
 */
export type PairResult = {
  paired: boolean
  serverId: string
  deviceName: string
}

/** Every command failure arrives in this shape. */
export type AppError = {
  code: string
  message: string
  retryable: boolean
}

export function isAppError(value: unknown): value is AppError {
  return (
    typeof value === "object" &&
    value !== null &&
    typeof (value as AppError).code === "string" &&
    typeof (value as AppError).message === "string"
  )
}

/**
 * Normalize anything thrown by `invoke` into an `AppError`, so no screen has
 * to handle a bare string or a raw exception.
 */
export function toAppError(value: unknown): AppError {
  if (isAppError(value)) return value
  return {
    code: "INTERNAL_ERROR",
    message: "Something went wrong connecting to Arciin.",
    retryable: true,
  }
}
