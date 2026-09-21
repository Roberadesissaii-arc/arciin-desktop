/**
 * Typed wrappers over the native commands.
 *
 * This is the complete list of what the UI can ask the native layer to do.
 * There is no `getCredential`, and there never should be: React's job is to
 * say *which* server to pair or connect to, not to hold what makes it work.
 */

import { invoke } from "@tauri-apps/api/core"

import type { PairResult, SavedServer, VerifiedServer } from "@/types"
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

/**
 * Listen for the native layer telling us this computer was disconnected.
 *
 * Emitted after it has already cleared this server's credentials, so the
 * UI's only job is to explain what happened.
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
