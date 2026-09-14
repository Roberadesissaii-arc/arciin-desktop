/**
 * The onboarding state machine.
 *
 * One store holds the whole first-run journey, because every screen is a view
 * of the same question - "which Arciin server is this computer connected to,
 * and how far along are we?" - and splitting that across components is what
 * makes connection flows drift out of sync with reality.
 */

import { create } from "zustand"

import * as ipc from "@/lib/ipc"
import type { AppError, SavedServer, VerifiedServer } from "@/types"
import { toAppError } from "@/types"

export type Step =
  /** Reading saved servers; the first paint after launch. */
  | "booting"
  /** mDNS browse in flight. */
  | "searching"
  /** Search finished - with or without results. */
  | "results"
  /** Manual address entry. */
  | "manual"
  /** Pairing code entry for a chosen, verified server. */
  | "pairing"
  /** Device bootstrap and handover to the Arciin window. */
  | "connecting"
  /** The Arciin window is open; this one is hidden behind it. */
  | "connected"
  /** Offering computer backup, after pairing and sign-in. */
  | "protectFolders"
  | "backupCenter"
  /** This computer was disconnected server-side. */
  | "disconnected"
  /** Native backup status and settings. */
  | "backupSettings"

/** Sub-states of "connecting", so the UI can name what is happening. */
export type ConnectPhase = "verifying" | "authorizing" | "securing" | "opening"

type State = {
  step: Step
  /** Populated once a server has been chosen but not yet paired. */
  target: VerifiedServer | null
  found: VerifiedServer[]
  saved: SavedServer[]
  /** Set while an address or code is being checked. */
  busy: boolean
  error: AppError | null
  connectPhase: ConnectPhase
  deviceName: string
  /** Name of the server that disconnected us, for the explanation screen. */
  disconnectedServer: string | null
}

type Actions = {
  /** Decide what the first screen is. */
  boot: () => Promise<void>
  search: () => Promise<void>
  goManual: () => void
  goResults: () => void
  /** Verify a typed address and move to pairing. */
  submitAddress: (address: string) => Promise<void>
  /** Choose a discovered server. */
  chooseServer: (server: VerifiedServer) => void
  setDeviceName: (name: string) => void
  submitCode: (code: string) => Promise<void>
  /**
   * Connect to a server already paired on this computer.
   *
   * `silent` is the launch path: keep the window hidden unless it fails, so a
   * working reconnect never flashes the setup shell.
   */
  connect: (serverId: string, options?: { silent?: boolean }) => Promise<void>
  /** Offer computer backup, if the server supports it and someone is signed in. */
  offerBackup: () => Promise<void>
  /** Dismiss the backup offer. Backup is optional. */
  skipBackup: () => void
  /** The native layer detected that this computer is no longer trusted. */
  deviceRevoked: (serverId: string) => Promise<void>
  /** Open the native backup status screen. */
  showBackupSettings: () => void
  /** Leave the native backup UI and return to the running Arciin window. */
  closeBackupUi: () => void
  /** The Arciin page asked for the folder-protection screen. */
  openBackupSetup: () => Promise<void>
  forget: (serverId: string) => Promise<void>
  clearError: () => void
  reset: () => void
}

const initial: State = {
  step: "booting",
  target: null,
  found: [],
  saved: [],
  busy: false,
  error: null,
  connectPhase: "verifying",
  deviceName: "",
  disconnectedServer: null,
}

/**
 * Show the onboarding window, once.
 *
 * The window is hidden at launch so an already-paired computer can reconnect
 * without the setup shell flashing past. Every screen that needs a person
 * calls this; screens that are only ever passed through on the way to Arciin
 * deliberately do not.
 */
function ensureVisible() {
  // Deliberately not latched. It used to be, which meant the backup UI could
  // be opened exactly once per run: every later request set the route while
  // the window stayed hidden, and nothing appeared. `reveal_onboarding` is a
  // no-op when the window is already visible, so calling it is cheap.
  void ipc.revealOnboarding()
}

export const useOnboarding = create<State & Actions>((set, get) => ({
  ...initial,

  async boot() {
    try {
      const [saved, deviceName] = await Promise.all([
        ipc.savedServers(),
        ipc.suggestedDeviceName(),
      ])
      set({ saved, deviceName })

      // Second launch: reconnect without asking, so a paired computer opens
      // straight into Arciin. A revoked server is skipped - it needs a person.
      //
      // The window is deliberately left hidden for this path. If the reconnect
      // works, the first thing the user sees is Arciin itself; if it fails,
      // `connect` reveals the window to show why.
      const usable = saved.find((server) => !server.revoked)
      if (usable) {
        await get().connect(usable.serverId, { silent: true })
        return
      }
    } catch (error) {
      // A failure to read saved state must not trap the user on a dead
      // screen; fall through to the normal search.
      set({ error: toAppError(error) })
    }

    await get().search()
  },

  async search() {
    ensureVisible()
    set({ step: "searching", error: null, found: [] })
    try {
      const found = await ipc.discoverServers()
      set({ found, step: "results" })
    } catch (error) {
      // Discovery failing is not fatal: the manual path always works.
      set({ found: [], step: "results", error: toAppError(error) })
    }
  },

  goManual() {
    ensureVisible()
    set({ step: "manual", error: null })
  },

  goResults() {
    ensureVisible()
    set({ step: "results", error: null, target: null })
  },

  async submitAddress(address) {
    ensureVisible()
    set({ busy: true, error: null })
    try {
      const target = await ipc.verifyServer(address)
      set({ target, step: "pairing", busy: false })
    } catch (error) {
      set({ error: toAppError(error), busy: false })
    }
  },

  chooseServer(server) {
    ensureVisible()
    set({ target: server, step: "pairing", error: null })
  },

  setDeviceName(deviceName) {
    set({ deviceName })
  },

  async submitCode(code) {
    const { target, deviceName } = get()
    if (!target) return

    set({ busy: true, error: null })
    try {
      const result = await ipc.pairServer({
        address: target.baseUrl,
        code,
        deviceName,
      })
      set({ busy: false })
      await get().connect(result.serverId)
    } catch (error) {
      set({ error: toAppError(error), busy: false })
    }
  },

  async connect(serverId, options) {
    set({ step: "connecting", connectPhase: "verifying", error: null })
    if (!options?.silent) ensureVisible()

    // The native side walks verify -> authorize -> secure -> open. It reports
    // each stage in the log rather than over IPC, so the phases here are a
    // paced narration of a single call. They are honest about the order of
    // work, not a fake progress bar: the call either completes or errors.
    const phases: ConnectPhase[] = ["authorizing", "securing", "opening"]
    let index = 0
    const ticker = window.setInterval(() => {
      if (index < phases.length) {
        set({ connectPhase: phases[index] })
        index += 1
      }
    }, 700)

    try {
      await ipc.connectServer(serverId)
      set({ step: "connected" })
      const saved = await ipc.savedServers()
      set({ saved })
    } catch (error) {
      const appError = toAppError(error)
      const saved = await ipc.savedServers().catch(() => get().saved)
      // Whatever went wrong, the user now has to see it.
      ensureVisible()
      set({
        error: appError,
        saved,
        // A revoked or unknown device has to go back to pairing; anything
        // else can be retried from the server list.
        step: "results",
      })
    } finally {
      window.clearInterval(ticker)
    }
  },

  async forget(serverId) {
    set({ busy: true, error: null })
    try {
      await ipc.forgetServer(serverId)
      const saved = await ipc.savedServers()
      set({ saved, busy: false, target: null })
      await get().search()
    } catch (error) {
      set({ error: toAppError(error), busy: false })
    }
  },

  async offerBackup() {
    try {
      const availability = await ipc.backupAvailability()
      // Three conditions, all required: the server can do backup, a person is
      // signed in to authorize it, and it is not already set up here.
      if (availability.supported && availability.signedIn && !availability.enabled) {
        ensureVisible()
        set({ step: "protectFolders", error: null })
      }
    } catch {
      // Backup is an optional extra; failing to offer it must never disturb a
      // working connection.
    }
  },

  skipBackup() {
    set({ step: "connected" })
  },

  /**
   * Open the native backup UI, choosing the screen from real profile state.
   *
   * The server's sentinel is named `.../backup/setup`, but it means "open the
   * backup UI", not "always show the wizard". Someone whose backup has run for
   * weeks must not be handed first-run setup — that reads as though their
   * configuration had been lost.
   */
  async openBackupSetup() {
    ensureVisible()
    try {
      const state = await ipc.backupState()
      // An existing profile — healthy or in error — belongs in the Backup
      // Center; the error is something to see there, not a reason to set up
      // again. Only a genuinely absent profile opens the wizard.
      set({ step: state.enabled ? "backupCenter" : "protectFolders", error: null })
    } catch {
      set({ step: "protectFolders", error: null })
    }
  },

  /**
   * Leave the native backup UI.
   *
   * Native, not just a route change: the old version only set the step, so the
   * onboarding window stayed on top of a perfectly healthy Arciin window
   * showing its own status copy, which read as a restart.
   */
  closeBackupUi() {
    set({ step: "connected" })
    void ipc.closeBackupUi()
  },

  showBackupSettings() {
    ensureVisible()
    set({ step: "backupSettings", error: null })
  },

  async deviceRevoked(serverId) {
    // The native side has already stopped backup and cleared this server's
    // credentials. All that is left is to say so, and to name the server if we
    // still know it.
    const saved = await ipc.savedServers().catch(() => get().saved)
    const server = saved.find((entry) => entry.serverId === serverId)
    ensureVisible()
    set({
      saved,
      step: "disconnected",
      disconnectedServer: server?.name ?? null,
      target: null,
      error: null,
    })
  },

  clearError() {
    set({ error: null })
  },

  reset() {
    set({ ...initial, step: "results" })
  },
}))
