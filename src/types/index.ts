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

// --- Computer backup ----------------------------------------------------

/** Whether backup can be offered for the connected server. */
export type BackupAvailability = {
  /** The server advertises computerBackup at a version this build implements. */
  supported: boolean
  /** This computer already has a backup profile there. */
  enabled: boolean
  /** A signed-in user session exists in the Arciin window. */
  signedIn: boolean
}

/** A Windows known folder that can be protected. */
export type ProtectableFolder = {
  /**
   * What identifies this folder across the IPC boundary.
   *
   * A known folder is its own kind; a folder chosen with the Windows picker
   * gets an opaque `custom:<uuid>` handle. Either way the renderer never sees
   * or sends a Windows path.
   */
  id: string
  /** `DESKTOP`, `DOCUMENTS`, … or `CUSTOM` */
  kind: string
  displayName: string
  selectedByDefault: boolean
  fileCount?: number
  totalBytes?: number
}

/** Size and count for one folder, once measured. */
export type ScanSummary = {
  fileCount: number
  folderCount: number
  totalBytes: number
  skipped: number
  cancelled: boolean
}

/**
 * Where turning backup on has got to.
 *
 * Deliberately not a boolean: the awaited part of activation is fast, and the
 * part after it (walking folders, draining the queue) is open-ended. One
 * `loading` flag could not tell those apart, so a slow scan and a wedged call
 * looked identical.
 */
export type ActivationStage =
  | { state: "IDLE" }
  | { state: "AUTHORIZING" }
  | { state: "CREATING_PROFILE" }
  | { state: "CREATING_ROOTS" }
  | { state: "SCANNING" }
  | { state: "UPLOADING" }
  | { state: "ACTIVE" }
  | { state: "ERROR"; code: string; message: string }

export type BackupHealth = "UP_TO_DATE" | "SYNCING" | "PAUSED" | "OFFLINE" | "ERROR"

/** What the Arciin server's disk looks like. Nulls mean "could not probe". */
export type ServerStorage = {
  usageBytes?: number | null
  totalBytes?: number | null
  availableBytes?: number | null
}

export type BackupStatus = {
  health: BackupHealth
  filesSynced: number
  filesOutstanding: number
  filesFailed: number
  bytesSynced: number
  bytesOutstanding: number
  paused: boolean
  lastError?: string
}

export type ProtectedRoot = {
  id: string
  kind: string
  displayName: string
  enabled: boolean
  /**
   * Where the folder is on this PC.
   *
   * Native-only. This reaches the onboarding window, which is our own bundled
   * UI; it is never sent to the server and never reaches the webview showing
   * the server's page, which has no IPC at all.
   */
  localPath: string
  fileCount: number
  pending: number
  failed: number
  bytesSynced: number
}

/**
 * Backup state for the connected server.
 *
 * Note what is absent: the `arcsync_` credential. No command returns it, and
 * there is no field here it could arrive in.
 */
/**
 * Where this computer stands with its server's backup.
 *
 * Three states, not two. `NOT_SET_UP` and `DISABLED` used to look identical to
 * the UI, which is what made stopping backup a one-way door: a computer that
 * had been switched off was indistinguishable from one that had never been set
 * up, so the only route back was to start over and build a second tree.
 */
export type BackupLifecycle = "NOT_SET_UP" | "ACTIVE" | "DISABLED"

export type BackupState = {
  enabled: boolean
  lifecycle: BackupLifecycle
  status?: BackupStatus
  roots: ProtectedRoot[]
  activation?: ActivationStage
  /** When a file last landed successfully, RFC 3339. */
  lastBackupAt?: string
}
