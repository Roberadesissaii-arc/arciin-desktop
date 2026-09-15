/**
 * Typed wrappers over the native commands.
 *
 * This is the complete list of what the UI can ask the native layer to do.
 * There is no `getCredential`, and there never should be: React's job is to
 * say *which* server to pair or connect to, not to hold what makes it work.
 */

import { invoke } from "@tauri-apps/api/core"

import type {
  ActivationStage,
  BackupAvailability,
  BackupState,
  PairResult,
  ProtectableFolder,
  SavedServer,
  ScanSummary,
  ServerStorage,
  VerifiedServer,
} from "@/types"
import { toAppError } from "@/types"

async function call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(command, args)
  } catch (error) {
    throw toAppError(error)
  }
}

/** Browse the LAN. An empty array is a normal, non-error result. */
export function discoverServers(): Promise<VerifiedServer[]> {
  return call<VerifiedServer[]>("discover_servers")
}

/** Check a typed address and return the server behind it. */
export function verifyServer(address: string): Promise<VerifiedServer> {
  return call<VerifiedServer>("verify_server", { address })
}

/** Servers this computer has already paired with. */
export function savedServers(): Promise<SavedServer[]> {
  return call<SavedServer[]>("saved_servers")
}

/** A sensible default device name, from the Windows hostname. */
export function suggestedDeviceName(): Promise<string> {
  return call<string>("suggested_device_name")
}

/** Claim a pairing code. The credential it returns stays inside Rust. */
export function pairServer(args: {
  address: string
  code: string
  deviceName: string
}): Promise<PairResult> {
  return call<PairResult>("pair_server", args)
}

/** Authenticate the device and open the real Arciin interface. */
export function connectServer(serverId: string): Promise<void> {
  return call<void>("connect_server", { serverId })
}

/** Forget a server on this computer. Does not revoke it server-side. */
export function forgetServer(serverId: string): Promise<void> {
  return call<void>("forget_server", { serverId })
}

// --- Computer backup ----------------------------------------------------
//
// None of these take a server id: the native layer acts on the connection it
// verified itself, so a renderer can never point uploads at another host.

/** Can backup be offered for the connected server, and is it already on? */
export function backupAvailability(): Promise<BackupAvailability> {
  return call<BackupAvailability>("backup_availability")
}

/** Known folders on this machine, without sizes yet. */
export function backupFolders(): Promise<ProtectableFolder[]> {
  return call<ProtectableFolder[]>("backup_folders")
}

/** Measure one folder. Cancels any measurement already running. */
export function backupMeasureFolder(id: string): Promise<ScanSummary> {
  return call<ScanSummary>("backup_measure_folder", { id })
}

/** Abandon an in-flight measurement, e.g. when the screen closes. */
export function backupCancelMeasuring(): Promise<void> {
  return call<void>("backup_cancel_measuring")
}

/**
 * Open the native Windows folder picker.
 *
 * Takes no argument — there is no way to ask for a particular folder, only for
 * the dialog. Resolves to `null` if the person cancelled. The folder's path
 * stays in Rust; what comes back is a handle and a name.
 */
export function backupPickFolder(): Promise<ProtectableFolder | null> {
  return call<ProtectableFolder | null>("backup_pick_folder")
}

/** Turn backup on for the chosen folders. Returns state, never a credential. */
export function backupEnable(selections: string[]): Promise<BackupState> {
  return call<BackupState>("backup_enable", { selections })
}

/** How much room the Arciin server has. */
export function backupServerStorage(): Promise<ServerStorage> {
  return call<ServerStorage>("backup_server_storage")
}

export function backupState(): Promise<BackupState> {
  return call<BackupState>("backup_state")
}

export function backupPause(): Promise<void> {
  return call<void>("backup_pause")
}

export function backupResume(): Promise<void> {
  return call<void>("backup_resume")
}

/**
 * Listen for activation moving between stages.
 *
 * The native side owns this sequence, because the slow parts — walking the
 * folders, draining the queue — happen there. Without it the screen would be
 * back to guessing from a single boolean.
 */
export async function onBackupActivation(
  handler: (stage: ActivationStage) => void,
): Promise<() => void> {
  try {
    const { listen } = await import("@tauri-apps/api/event")
    return await listen<ActivationStage>("arciin://backup-activation", (event) => {
      handler(event.payload)
    })
  } catch {
    // Not running inside Tauri (a browser dev preview). Nothing to listen to.
    return () => {}
  }
}

/** Stop protecting one folder. Keeps the profile, pairing and local files. */
export function backupRemoveRoot(rootId: string): Promise<BackupState> {
  return call<BackupState>("backup_remove_root", { rootId })
}

/**
 * Protect a folder again that was switched off.
 *
 * The same root, reactivated — not a new one. Nothing is duplicated on the
 * server and the files already stored under it are not re-sent.
 */
export function backupResumeRoot(rootId: string): Promise<BackupState> {
  return call<BackupState>("backup_resume_root", { rootId })
}

/**
 * Accept a large change that was held for safety, and let the folder resume.
 *
 * Nothing is removed by this call. It only lets the folder be read again; if
 * the files have come back, the next check finds them and removes nothing.
 */
export function backupResolveSafetyHold(rootId: string): Promise<BackupState> {
  return call<BackupState>("backup_resolve_safety_hold", { rootId })
}

/** Read every protected folder again now, rather than waiting for the timer. */
export function backupRescan(): Promise<void> {
  return call<void>("backup_rescan")
}

/** Open a protected folder in File Explorer, resolved natively by id. */
export function backupOpenRoot(rootId: string): Promise<void> {
  return call<void>("backup_open_root", { rootId })
}

/**
 * Close the native backup surface and return to the running Arciin window.
 *
 * Native on purpose: changing a React route alone left the onboarding window
 * covering a healthy Arciin window, which read as a restart.
 */
export function closeBackupUi(): Promise<void> {
  return call<void>("close_backup_ui")
}

/** Size the window for the Backup Center rather than the first-run shell. */
export function sizeForBackupCenter(): Promise<void> {
  return call<void>("size_for_backup_center")
}

/** Turn backup off on this computer. Local files are never touched. */
export function backupForget(): Promise<void> {
  return call<void>("backup_forget")
}

/**
 * Turn backup back on for a computer that was switched off.
 *
 * Reuses the existing profile, so the computer keeps its identity and its
 * stored files. Folders come back listed but not protected; each is resumed
 * deliberately with {@link backupResumeRoot}.
 */
export function backupReenable(): Promise<BackupState> {
  return call<BackupState>("backup_reenable")
}

/**
 * Listen for the native layer telling us this computer was disconnected.
 *
 * Emitted after it has already stopped backup and cleared this server's
 * credentials, so the UI's only job is to explain what happened.
 */
export async function onDeviceRevoked(
  handler: (serverId: string) => void,
): Promise<() => void> {
  try {
    const { listen } = await import("@tauri-apps/api/event")
    return await listen<string>("arciin://device-revoked", (event) => {
      handler(event.payload)
    })
  } catch {
    // Not running inside Tauri (a browser dev preview). Nothing to listen to.
    return () => {}
  }
}

/**
 * Ask the native layer to show the onboarding window.
 *
 * It launches hidden and stays hidden through an automatic reconnect, so a
 * paired computer goes straight to Arciin. Call this the moment there is
 * something the user must see. Safe to call repeatedly.
 */
export function revealOnboarding(): Promise<void> {
  return call<void>("reveal_onboarding")
}

/**
 * Listen for the Arciin page asking to open native backup setup.
 *
 * The native layer has already verified the message came from the connected
 * server and is the one allowlisted action, so there is no payload to inspect
 * here — the event firing *is* the whole message.
 */
export async function onOpenBackupSetup(handler: () => void): Promise<() => void> {
  try {
    const { listen } = await import("@tauri-apps/api/event")
    return await listen("arciin://open-backup-setup", () => handler())
  } catch {
    // Not running inside Tauri. A browser has no native screen to open.
    return () => {}
  }
}
