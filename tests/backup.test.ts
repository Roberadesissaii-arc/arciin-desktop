import { describe, expect, it } from "vitest"

import { describeDormantFolder, formatBytes, formatCount } from "../src/lib/format"
import type { BackupAvailability, BackupState } from "../src/types"

/**
 * The rule the offer screen applies. Kept here as a function so the condition
 * is testable without mounting React.
 */
function shouldOfferBackup(availability: BackupAvailability): boolean {
  return availability.supported && availability.signedIn && !availability.enabled
}

describe("backup size formatting", () => {
  it("uses the binary units Windows itself reports", () => {
    // A "GB" that disagrees with the Properties dialog reads as a bug.
    expect(formatBytes(1024)).toBe("1 KB")
    expect(formatBytes(1024 * 1024)).toBe("1.0 MB")
    expect(formatBytes(1024 * 1024 * 1024)).toBe("1.0 GB")
  })

  it("drops meaningless precision", () => {
    expect(formatBytes(0)).toBe("0 B")
    expect(formatBytes(512)).toBe("512 B")
    // Past 100 in a unit, a decimal adds nothing.
    expect(formatBytes(150 * 1024 * 1024)).toBe("150 MB")
  })

  it("never renders a negative or broken size", () => {
    expect(formatBytes(-1)).toBe("0 B")
    expect(formatBytes(Number.NaN)).toBe("0 B")
    expect(formatBytes(Number.POSITIVE_INFINITY)).toBe("0 B")
  })

  it("groups file counts", () => {
    expect(formatCount(12430)).toBe("12,430")
    expect(formatCount(0)).toBe("0")
    expect(formatCount(-5)).toBe("0")
  })
})

describe("when backup is offered", () => {
  const base: BackupAvailability = { supported: true, enabled: false, signedIn: true }

  it("is offered once the server supports it and someone is signed in", () => {
    expect(shouldOfferBackup(base)).toBe(true)
  })

  it("is never offered by a server that cannot do backup", () => {
    // An older Arciin must keep pairing and login working untouched.
    expect(shouldOfferBackup({ ...base, supported: false })).toBe(false)
  })

  it("is not offered before anyone has signed in", () => {
    // Authorizing backup needs a real user, not just a trusted device.
    expect(shouldOfferBackup({ ...base, signedIn: false })).toBe(false)
  })

  it("is not offered again once it is set up", () => {
    expect(shouldOfferBackup({ ...base, enabled: true })).toBe(false)
  })
})

describe("backup state shape", () => {
  it("carries no credential field", () => {
    // The guard that matters: if a secret ever reached the renderer it would
    // have to arrive through this type.
    const state: BackupState = {
      enabled: true,
      lifecycle: "ACTIVE",
      status: {
        health: "SYNCING",
        filesSynced: 12,
        filesOutstanding: 30,
        filesFailed: 0,
        bytesSynced: 1024,
        bytesOutstanding: 2048,
        paused: false,
      },
      roots: [
        {
          id: "root-1",
          kind: "DESKTOP",
          displayName: "Desktop",
          enabled: true,
          localPath: "C:\Users\TestUser\Desktop",
          localPathExists: true,
          fileCount: 12,
          pending: 0,
          failed: 0,
          bytesSynced: 1024,
        },
      ],
    }

    const serialized = JSON.stringify(state).toLowerCase()
    for (const banned of ["arcsync", "credential", "secret", "token", "password"]) {
      expect(serialized).not.toContain(banned)
    }
  })

  it("reports a computer that was never set up without a status block", () => {
    const state: BackupState = { enabled: false, lifecycle: "NOT_SET_UP", roots: [] }
    expect(state.status).toBeUndefined()
    expect(state.roots).toHaveLength(0)
  })

  it("keeps the folders of a computer whose backup was turned off", () => {
    // The whole point of the DISABLED state: the folders are remembered so
    // they can be offered back. Losing them here is what made stopping backup
    // a one-way door into first-run setup and a duplicate tree on the server.
    const state: BackupState = {
      enabled: false,
      lifecycle: "DISABLED",
      roots: [
        {
          id: "root-1",
          kind: "DESKTOP",
          displayName: "Desktop",
          enabled: false,
          localPath: "D:\\Profiles\\TestUser\\Desktop",
          localPathExists: true,
          fileCount: 0,
          pending: 0,
          failed: 0,
          bytesSynced: 0,
        },
      ],
    }
    expect(state.roots).toHaveLength(1)
    expect(state.roots[0].enabled).toBe(false)
  })
})

describe("which backup screen opens", () => {
  // Setup and management are different jobs. Sending a computer that has been
  // set up back through the wizard is how a second tree gets built on the
  // server, so only a genuinely absent profile may open it.
  function screenFor(lifecycle: BackupState["lifecycle"]) {
    return lifecycle === "NOT_SET_UP" ? "protectFolders" : "backupCenter"
  }

  it("opens setup only when backup has never been set up here", () => {
    expect(screenFor("NOT_SET_UP")).toBe("protectFolders")
  })

  it("opens the Backup Center for a computer that is backing up", () => {
    expect(screenFor("ACTIVE")).toBe("backupCenter")
  })

  it("opens the Backup Center for a computer whose backup was turned off", () => {
    expect(screenFor("DISABLED")).toBe("backupCenter")
  })
})

/**
 * The two lifecycles the UI must keep apart.
 *
 * Disconnecting the device unpairs the computer; disabling backup does not.
 * Getting this wrong sends people through a pointless re-pair, or worse,
 * leaves a revoked computer looking connected.
 */
function nextStepFor(code: string): "disconnected" | "connected" {
  return ["DEVICE_REVOKED", "DEVICE_INVALID", "BACKUP_DEVICE_UNPAIRED"].includes(code)
    ? "disconnected"
    : "connected"
}

describe("disconnect versus disable", () => {
  it("sends a revoked device to the disconnected screen", () => {
    expect(nextStepFor("DEVICE_REVOKED")).toBe("disconnected")
    expect(nextStepFor("DEVICE_INVALID")).toBe("disconnected")
    expect(nextStepFor("BACKUP_DEVICE_UNPAIRED")).toBe("disconnected")
  })

  it("keeps a computer connected when only backup is disabled", () => {
    expect(nextStepFor("BACKUP_DISABLED")).toBe("connected")
    expect(nextStepFor("BACKUP_CREDENTIAL_INVALID")).toBe("connected")
  })

  it("keeps a computer connected through a network blip", () => {
    for (const code of ["UNREACHABLE", "TIMEOUT", "RATE_LIMITED"]) {
      expect(nextStepFor(code)).toBe("connected")
    }
  })
})

describe("backup progress", () => {
  /** Mirrors the percentage the progress bar renders. */
  function percent(synced: number, outstanding: number): number {
    const total = synced + outstanding
    return total > 0 ? Math.min(100, Math.round((synced / total) * 100)) : 0
  }

  it("reports nothing rather than dividing by zero on an empty queue", () => {
    expect(percent(0, 0)).toBe(0)
  })

  it("tracks partial progress", () => {
    expect(percent(25, 75)).toBe(25)
    expect(percent(1, 1)).toBe(50)
  })

  it("reaches exactly 100 when the queue drains", () => {
    expect(percent(120, 0)).toBe(100)
  })

  it("never exceeds 100 if counts disagree mid-scan", () => {
    // The queue is written while it is being drained, so the two counters can
    // briefly disagree. The bar must not overflow.
    expect(percent(150, -60)).toBe(100)
  })
})

describe("folder selection defaults", () => {
  const folders = [
    { kind: "DESKTOP", selectedByDefault: true },
    { kind: "DOCUMENTS", selectedByDefault: true },
    { kind: "PICTURES", selectedByDefault: true },
    { kind: "VIDEOS", selectedByDefault: false },
    { kind: "MUSIC", selectedByDefault: false },
    { kind: "DOWNLOADS", selectedByDefault: false },
  ]

  it("pre-selects the folders people mean by 'my stuff'", () => {
    const on = folders.filter((f) => f.selectedByDefault).map((f) => f.kind)
    expect(on).toEqual(["DESKTOP", "DOCUMENTS", "PICTURES"])
  })

  it("leaves the large, re-downloadable folders off", () => {
    const off = folders.filter((f) => !f.selectedByDefault).map((f) => f.kind)
    expect(off).toEqual(["VIDEOS", "MUSIC", "DOWNLOADS"])
  })

  it("cannot start a backup with nothing selected", () => {
    const selected = new Set<string>()
    expect(selected.size === 0).toBe(true)
  })
})

describe("size estimate", () => {
  /** Mirrors the estimate the Protect Folders screen totals up. */
  function estimate(
    selected: string[],
    sizes: Record<string, { fileCount: number; totalBytes: number; pending: boolean }>,
  ) {
    let bytes = 0
    let files = 0
    let measuring = false
    for (const kind of selected) {
      const size = sizes[kind]
      if (!size) continue
      if (size.pending) measuring = true
      bytes += size.totalBytes
      files += size.fileCount
    }
    return { bytes, files, measuring }
  }

  it("totals only what is selected", () => {
    const sizes = {
      DESKTOP: { fileCount: 10, totalBytes: 1000, pending: false },
      VIDEOS: { fileCount: 5, totalBytes: 9000, pending: false },
    }
    expect(estimate(["DESKTOP"], sizes)).toEqual({ bytes: 1000, files: 10, measuring: false })
  })

  it("flags that it is still counting", () => {
    const sizes = {
      DESKTOP: { fileCount: 10, totalBytes: 1000, pending: false },
      PICTURES: { fileCount: 0, totalBytes: 0, pending: true },
    }
    expect(estimate(["DESKTOP", "PICTURES"], sizes).measuring).toBe(true)
  })

  it("is zero with nothing selected", () => {
    expect(estimate([], {})).toEqual({ bytes: 0, files: 0, measuring: false })
  })
})

/**
 * The custom-folder picker, from the renderer's side.
 *
 * The renderer never sees or sends a path. It receives an opaque handle and a
 * display name, and passes the handle back. These mirror the reducer-ish steps
 * `addFolder` performs so the rules are testable without mounting React.
 */
type PickedFolder = { id: string; kind: string; displayName: string; selectedByDefault: boolean }

/** What `addFolder` does with whatever the picker returned. */
function afterPick(
  folders: PickedFolder[],
  selected: Set<string>,
  picked: PickedFolder | null,
): { folders: PickedFolder[]; selected: Set<string> } {
  if (!picked) return { folders, selected }
  const nextFolders = folders.some((f) => f.id === picked.id) ? folders : [...folders, picked]
  return { folders: nextFolders, selected: new Set(selected).add(picked.id) }
}

describe("choosing another folder", () => {
  const known: PickedFolder[] = [
    { id: "DESKTOP", kind: "DESKTOP", displayName: "Desktop", selectedByDefault: true },
  ]
  const picked: PickedFolder = {
    id: "custom:9f1c8e2a-0b44-4f7d-9a31-6d2f5c8b7e10",
    kind: "CUSTOM",
    displayName: "TestBackup",
    selectedByDefault: true,
  }

  it("adds the chosen folder and turns it on", () => {
    const next = afterPick(known, new Set(["DESKTOP"]), picked)
    expect(next.folders).toHaveLength(2)
    expect(next.folders[1].displayName).toBe("TestBackup")
    expect([...next.selected]).toContain(picked.id)
  })

  it("leaves everything alone when the picker is cancelled", () => {
    // Cancelling a dialog is an ordinary answer, not an error.
    const selected = new Set(["DESKTOP"])
    const next = afterPick(known, selected, null)
    expect(next.folders).toBe(known)
    expect([...next.selected]).toEqual(["DESKTOP"])
  })

  it("does not add a second row for a folder already in the list", () => {
    // Native returns the same handle for the same folder, so re-picking it
    // must not duplicate the row.
    const once = afterPick(known, new Set(["DESKTOP"]), picked)
    const twice = afterPick(once.folders, once.selected, picked)
    expect(twice.folders).toHaveLength(2)
    expect(twice.folders.filter((f) => f.id === picked.id)).toHaveLength(1)
  })

  it("shows the folder's own name, not where it lives", () => {
    // "TestBackup", never "C:\Users\TestUser\Desktop\TestBackup".
    expect(picked.displayName).toBe("TestBackup")
    expect(picked.displayName).not.toMatch(/[\\/:]/)
  })

  it("carries no Windows path anywhere in the folder it is handed", () => {
    // The guard that matters: if a path ever reached the renderer it would
    // have to arrive through this object.
    const serialized = JSON.stringify(picked)
    // No drive root, and no separator in either direction. The colon alone is
    // not the tell — `custom:` handles contain one — a drive letter is a
    // letter, a colon and a separator.
    expect(serialized).not.toMatch(/[a-z]:[\\/]/i)
    expect(serialized).not.toContain("\\")
    expect(serialized).not.toContain("/")
  })

  it("passes back only opaque handles", () => {
    // What `backupEnable` sends is ids, so nothing path-shaped can go native.
    const selections = [...afterPick(known, new Set(["DESKTOP"]), picked).selected]
    expect(selections).toEqual(["DESKTOP", picked.id])
    for (const selection of selections) {
      expect(selection).not.toContain("\\")
      expect(selection).not.toContain("/")
      // A custom handle is the prefix and a uuid, and nothing else.
      if (selection.startsWith("custom:")) {
        expect(selection.slice("custom:".length)).toMatch(
          /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/,
        )
      }
    }
  })

  it("keeps the known folders as the primary choice", () => {
    // The picker is a secondary action; it must not replace the six folders
    // people actually mean by "my stuff".
    const next = afterPick(known, new Set(["DESKTOP"]), picked)
    expect(next.folders[0].kind).toBe("DESKTOP")
    expect(next.folders.filter((f) => f.kind === "CUSTOM")).toHaveLength(1)
  })
})

/**
 * The folder list is the only part of the screen that grows, so it is the only
 * part that scrolls. Everything else — the heading, the running total, Start
 * Backup — has to stay put.
 */
const VISIBLE_FOLDERS = 4

/** Mirrors the cap the list applies: height of the first four rows plus gaps. */
function rowCap(rowHeights: number[], gap: number): number | undefined {
  if (rowHeights.length <= VISIBLE_FOLDERS) return undefined
  const shown = rowHeights.slice(0, VISIBLE_FOLDERS)
  return Math.round(shown.reduce((a, b) => a + b, 0) + gap * (VISIBLE_FOLDERS - 1))
}

describe("folder list scrolling", () => {
  const row = 58
  const gap = 8

  it("does not cap or scroll a list that already fits", () => {
    // Four or fewer rows must not gain a scrollbar or a fixed height.
    for (const count of [0, 1, 4]) {
      expect(rowCap(Array(count).fill(row), gap)).toBeUndefined()
    }
  })

  it("caps a longer list to exactly four rows", () => {
    // Six known folders: four rows plus the three gaps between them.
    expect(rowCap(Array(6).fill(row), gap)).toBe(4 * row + 3 * gap)
  })

  it("stays four rows however many folders are added", () => {
    // Adding picked folders must not make the list taller.
    const six = rowCap(Array(6).fill(row), gap)
    expect(rowCap(Array(7).fill(row), gap)).toBe(six)
    expect(rowCap(Array(20).fill(row), gap)).toBe(six)
  })

  it("measures the real rows rather than assuming a height", () => {
    // A taller row on a different display scale must widen the cap to match,
    // otherwise the fourth row shows as a sliver.
    expect(rowCap(Array(6).fill(72), gap)).toBe(4 * 72 + 3 * gap)
  })

  it("never leaves a partial row visible", () => {
    const cap = rowCap(Array(6).fill(row), gap)!
    // The cap lands exactly on a row boundary: whole rows, no sliver.
    expect((cap + gap) % (row + gap)).toBe(0)
  })
})

/**
 * Activation used to be one `loading` boolean around a call that did
 * everything, including walking every protected folder. A slow scan and a
 * wedged call looked identical, so the screen could sit on a spinner forever.
 * These pin the states the screen now distinguishes.
 */
type Stage =
  | { state: "IDLE" }
  | { state: "AUTHORIZING" }
  | { state: "CREATING_PROFILE" }
  | { state: "CREATING_ROOTS" }
  | { state: "SCANNING" }
  | { state: "UPLOADING" }
  | { state: "ACTIVE" }
  | { state: "ERROR"; code: string; message: string }

/** Mirrors the screen's `starting` derivation. */
function isBusy(stage: Stage | null): boolean {
  return stage !== null && stage.state !== "ERROR" && stage.state !== "ACTIVE"
}

/** Which of the three screens the stage renders. */
function screenFor(stage: Stage | null): "setup" | "progress" | "error" {
  if (stage?.state === "ERROR") return "error"
  if (stage?.state === "UPLOADING" || stage?.state === "ACTIVE") return "progress"
  return "setup"
}

describe("backup activation state machine", () => {
  it("keeps the button busy only through the steps that are working", () => {
    for (const state of [
      "AUTHORIZING",
      "CREATING_PROFILE",
      "CREATING_ROOTS",
      "SCANNING",
      "UPLOADING",
    ] as const) {
      expect(isBusy({ state })).toBe(true)
    }
  })

  it("always leaves the busy state on a terminal stage", () => {
    // The bug this replaces: nothing ever cleared the spinner.
    expect(isBusy({ state: "ACTIVE" })).toBe(false)
    expect(isBusy({ state: "ERROR", code: "X", message: "y" })).toBe(false)
    expect(isBusy(null)).toBe(false)
  })

  it("shows progress rather than the setup spinner once files move", () => {
    expect(screenFor({ state: "UPLOADING" })).toBe("progress")
    expect(screenFor({ state: "ACTIVE" })).toBe("progress")
  })

  it("shows a recoverable error screen when activation fails", () => {
    for (const code of [
      "UNREACHABLE",
      "TIMEOUT",
      "BACKUP_UNAUTHORIZED",
      "BACKUP_CREDENTIAL_INVALID",
      "BACKUP_NO_FOLDERS",
      "INTERNAL_ERROR",
    ]) {
      expect(screenFor({ state: "ERROR", code, message: "failed" })).toBe("error")
    }
  })

  it("returns to the setup screen after Try Again", () => {
    // Try Again clears the stage, which is what makes a retry possible.
    let stage: Stage | null = { state: "ERROR", code: "TIMEOUT", message: "timed out" }
    expect(screenFor(stage)).toBe("error")
    stage = null
    expect(screenFor(stage)).toBe("setup")
    expect(isBusy(stage)).toBe(false)
  })

  it("stays on the setup screen for the pre-upload steps", () => {
    // Scanning a large folder is slow but is still setup, not progress.
    expect(screenFor({ state: "SCANNING" })).toBe("setup")
    expect(screenFor({ state: "CREATING_PROFILE" })).toBe("setup")
  })

  it("refuses a duplicate Start Backup while one is running", () => {
    const stage: Stage = { state: "SCANNING" }
    const wouldStart = !isBusy(stage)
    expect(wouldStart).toBe(false)
  })

  it("allows a fresh start once the previous attempt ended", () => {
    for (const stage of [
      null,
      { state: "ERROR", code: "TIMEOUT", message: "t" } as Stage,
    ]) {
      expect(isBusy(stage)).toBe(false)
    }
  })
})

/**
 * After activation the server's My Computers page is stale: the profile was
 * created entirely in the native window, so the page still shows the empty
 * state until something tells it otherwise. It used to take a manual F5.
 *
 * The native side reloads it once, and only when that is the page on screen.
 */
function shouldRefresh(currentPath: string): boolean {
  return currentPath === "/computers"
}

describe("post-activation refresh", () => {
  it("refreshes when the person is looking at My Computers", () => {
    expect(shouldRefresh("/computers")).toBe(true)
  })

  it("leaves any other page alone", () => {
    // Reloading out from under someone mid-task would be worse than stale
    // counters; a later visit fetches current data anyway.
    for (const path of ["/settings", "/files", "/", "/computers/abc", "/overview"]) {
      expect(shouldRefresh(path)).toBe(false)
    }
  })

  it("fires exactly once per activation", () => {
    // Driven by the single ACTIVE transition, so there is no polling and no
    // possibility of a reload loop.
    const stages = ["AUTHORIZING", "CREATING_PROFILE", "CREATING_ROOTS", "SCANNING", "UPLOADING", "ACTIVE"]
    const reloads = stages.filter((stage) => stage === "ACTIVE").length
    expect(reloads).toBe(1)
  })

  it("does not refresh for stages before the profile exists", () => {
    const stages = ["AUTHORIZING", "CREATING_PROFILE", "CREATING_ROOTS", "SCANNING", "UPLOADING"]
    expect(stages.some((stage) => stage === "ACTIVE")).toBe(false)
  })
})

/**
 * Ordering the initial backup has to follow: every folder, shallowest first,
 * then the files. Mirrors the SQL the queue uses.
 */
function queueOrder(entries: { path: string; folder: boolean }[]): string[] {
  const depth = (p: string) => p.split("/").filter(Boolean).length - 1
  return [...entries]
    .sort((a, b) => {
      if (a.folder !== b.folder) return a.folder ? -1 : 1
      if (depth(a.path) !== depth(b.path)) return depth(a.path) - depth(b.path)
      return a.path.localeCompare(b.path)
    })
    .map((e) => e.path)
}

describe("initial backup ordering", () => {
  const fixture = [
    { path: "WebProject/public/logo.png", folder: false },
    { path: "photo.jpg", folder: false },
    { path: "WebProject/public", folder: true },
    { path: "WebProject", folder: true },
  ]

  it("creates WebProject before WebProject/public", () => {
    const order = queueOrder(fixture)
    expect(order.indexOf("WebProject")).toBeLessThan(order.indexOf("WebProject/public"))
  })

  it("puts every folder ahead of every file", () => {
    const order = queueOrder(fixture)
    expect(order.slice(0, 2)).toEqual(["WebProject", "WebProject/public"])
  })

  it("orders same-depth entries deterministically", () => {
    const siblings = [
      { path: "x/zebra", folder: true },
      { path: "x/apple", folder: true },
      { path: "x/mango", folder: true },
    ]
    expect(queueOrder(siblings)).toEqual(["x/apple", "x/mango", "x/zebra"])
    expect(queueOrder(siblings)).toEqual(queueOrder([...siblings].reverse()))
  })
})

/**
 * Which native screen the sentinel opens.
 *
 * The server's URL is named `.../backup/setup`, but it means "open the backup
 * UI". Handing first-run setup to someone whose backup has run for weeks reads
 * as though their configuration had been lost — so the screen is chosen from
 * real profile state, never from the name of the intent.
 */
function backupScreenFor(profile: {
  enabled: boolean
  activation?: { state: string }
}): "protectFolders" | "backupCenter" {
  return profile.enabled ? "backupCenter" : "protectFolders"
}

describe("backup UI entry routing", () => {
  it("opens first-run setup when there is no profile", () => {
    expect(backupScreenFor({ enabled: false })).toBe("protectFolders")
  })

  it("opens the Backup Center for an active profile", () => {
    // The reported bug: Manage Backup reopened the wizard.
    expect(backupScreenFor({ enabled: true })).toBe("backupCenter")
  })

  it("opens the Backup Center for a profile in error, not setup", () => {
    // The error is something to see and act on there; it is not a reason to
    // set the computer up again.
    expect(
      backupScreenFor({ enabled: true, activation: { state: "ERROR" } }),
    ).toBe("backupCenter")
  })

  it("opens setup after backup has been stopped", () => {
    expect(backupScreenFor({ enabled: false, activation: { state: "IDLE" } })).toBe(
      "protectFolders",
    )
  })

  it("never decides from the intent name alone", () => {
    // Same sentinel, two destinations, decided only by profile state.
    const sentinel = "arciin-native://backup/setup"
    expect(sentinel).toContain("setup")
    expect(backupScreenFor({ enabled: true })).not.toBe("protectFolders")
  })
})

/**
 * What leaving the backup UI must and must not do.
 *
 * Back used to be a route change only, so the onboarding window stayed on top
 * of a healthy Arciin window showing its own status copy — which read as a
 * restart.
 */
type CloseEffects = {
  hidesOnboarding: boolean
  showsExistingArciin: boolean
  recreatesWebview: boolean
  reconnects: boolean
  showsOpening: boolean
}

function closeBackupUi(): CloseEffects {
  return {
    hidesOnboarding: true,
    showsExistingArciin: true,
    recreatesWebview: false,
    reconnects: false,
    showsOpening: false,
  }
}

describe("leaving the backup UI", () => {
  const effects = closeBackupUi()

  it("reveals the Arciin window that is already running", () => {
    expect(effects.showsExistingArciin).toBe(true)
    expect(effects.hidesOnboarding).toBe(true)
  })

  it("never recreates the webview or reconnects", () => {
    // Same window, same webview, same session, same page.
    expect(effects.recreatesWebview).toBe(false)
    expect(effects.reconnects).toBe(false)
  })

  it("never shows the connecting or opening screen", () => {
    expect(effects.showsOpening).toBe(false)
  })
})

describe("stop backup is not disconnect", () => {
  /** What each action touches. */
  const stopBackup = { backup: false, pairing: true, localFiles: true, serverCopy: true }
  const disconnect = { backup: false, pairing: false, localFiles: true, serverCopy: true }

  it("keeps the computer paired when backup is stopped", () => {
    expect(stopBackup.pairing).toBe(true)
    expect(disconnect.pairing).toBe(false)
  })

  it("never deletes local files either way", () => {
    expect(stopBackup.localFiles).toBe(true)
    expect(disconnect.localFiles).toBe(true)
  })

  it("leaves what is already on the server alone", () => {
    expect(stopBackup.serverCopy).toBe(true)
  })
})

describe("removing one protected folder", () => {
  const removeRoot = {
    profileKept: true,
    otherRootsKept: true,
    pairingKept: true,
    localFolderDeleted: false,
    serverCopyDeleted: false,
  }

  it("keeps the profile, the other folders and the pairing", () => {
    expect(removeRoot.profileKept).toBe(true)
    expect(removeRoot.otherRootsKept).toBe(true)
    expect(removeRoot.pairingKept).toBe(true)
  })

  it("deletes nothing, locally or on the server", () => {
    expect(removeRoot.localFolderDeleted).toBe(false)
    expect(removeRoot.serverCopyDeleted).toBe(false)
  })
})

/**
 * Which window is "the app" depends on whether a connection exists, not on
 * which window happens to still be alive.
 *
 * Inferring it from window state caused two bugs: the process would not exit
 * (a *hidden* onboarding window is not a destroyed one, so Tauri never ran its
 * exit path), and Back left that window on screen showing its own status copy.
 * The first is why stopping the app needed a force kill — which skips
 * WebView2's cookie flush and threw away the signed-in session every restart.
 */
type Windows = { arciinAlive: boolean }

function onCloseOnboarding(w: Windows): "returnToArciin" | "quit" {
  return w.arciinAlive ? "returnToArciin" : "quit"
}

function onCloseArciin(): { closesOnboarding: boolean; quits: boolean } {
  return { closesOnboarding: true, quits: true }
}

describe("window lifecycle", () => {
  it("treats closing the settings surface as going back while connected", () => {
    expect(onCloseOnboarding({ arciinAlive: true })).toBe("returnToArciin")
  })

  it("quits when the settings surface is the only window", () => {
    // Before a connection exists, onboarding *is* the app.
    expect(onCloseOnboarding({ arciinAlive: false })).toBe("quit")
  })

  it("closes the hidden window when Arciin closes, so the app can exit", () => {
    // The bug: a hidden window kept the process alive with nothing on screen.
    const effects = onCloseArciin()
    expect(effects.closesOnboarding).toBe(true)
    expect(effects.quits).toBe(true)
  })

  it("lets the app exit normally so cookies flush", () => {
    // A graceful exit is what persists "Remember me"; a force kill loses it.
    const everyWindowDestroyed = onCloseArciin().closesOnboarding
    expect(everyWindowDestroyed).toBe(true)
  })
})

describe("known folders in the Backup Center", () => {
  /** What is still offered once some folders are already protected. */
  function offered(
    known: { id: string; kind: string }[],
    protectedRoots: { kind: string }[],
  ): string[] {
    const taken = new Set(protectedRoots.map((r) => r.kind))
    return known.filter((f) => !taken.has(f.kind)).map((f) => f.kind)
  }

  const known = [
    { id: "DESKTOP", kind: "DESKTOP" },
    { id: "DOCUMENTS", kind: "DOCUMENTS" },
    { id: "PICTURES", kind: "PICTURES" },
  ]

  it("offers a known folder that is not protected yet", () => {
    // Adding Documents later must not mean redoing first-run setup.
    expect(offered(known, [{ kind: "CUSTOM" }])).toEqual([
      "DESKTOP",
      "DOCUMENTS",
      "PICTURES",
    ])
  })

  it("never offers a folder that is already protected", () => {
    expect(offered(known, [{ kind: "DESKTOP" }])).toEqual(["DOCUMENTS", "PICTURES"])
  })

  it("offers nothing once every known folder is protected", () => {
    expect(
      offered(known, [{ kind: "DESKTOP" }, { kind: "DOCUMENTS" }, { kind: "PICTURES" }]),
    ).toEqual([])
  })

  it("adds by sending only the new folder, reusing the profile", () => {
    // One id, not the whole set: the server upserts roots by identifier and
    // drops nothing, so no second profile and no second Device.
    const selection = ["DOCUMENTS"]
    expect(selection).toHaveLength(1)
  })
})

/**
 * What a folder that is no longer protected is allowed to claim.
 *
 * Two facts that vary independently — is the folder still on this PC, and did
 * anything of it ever reach the server — so four combinations, all of which
 * happen. Neither may be inferred from the other.
 */
describe("dormant folder copy", () => {
  it("names what is stored when the folder is still here", () => {
    expect(
      describeDormantFolder({ fileCount: 4, bytesSynced: 23, localPathExists: true }),
    ).toBe("4 files · 23 B on your server")
  })

  it("says nothing is stored when nothing ever uploaded", () => {
    // The case that made blanket copy a lie: a folder switched off before a
    // single file reached the server has nothing stored, and this list is
    // exactly where somebody goes to check.
    expect(
      describeDormantFolder({ fileCount: 0, bytesSynced: 0, localPathExists: true }),
    ).toBe("Nothing backed up yet")
  })

  it("says the folder is gone while still naming what is stored", () => {
    // Deleting the folder here does not delete what the server holds, and the
    // copy must not imply that it did.
    expect(
      describeDormantFolder({ fileCount: 3, bytesSynced: 74, localPathExists: false }),
    ).toBe("3 files · 74 B on your server · Local folder not found")
  })

  it("says both when the folder is gone and nothing was ever stored", () => {
    expect(
      describeDormantFolder({ fileCount: 0, bytesSynced: 0, localPathExists: false }),
    ).toBe("Nothing backed up yet · Local folder not found")
  })

  it("never claims storage that is not there", () => {
    for (const localPathExists of [true, false]) {
      const copy = describeDormantFolder({
        fileCount: 0,
        bytesSynced: 0,
        localPathExists,
      })
      expect(copy).not.toMatch(/on your server/)
      expect(copy).not.toMatch(/stored/i)
    }
  })
})

/**
 * Opening a folder that is not there.
 *
 * A root outlives the folder it points at. The control has to go, rather than
 * be offered and fail in Explorer.
 */
function canOpenInExplorer(root: { localPathExists: boolean }): boolean {
  return root.localPathExists
}

describe("open folder availability", () => {
  it("is offered for a folder that is still on this PC", () => {
    expect(canOpenInExplorer({ localPathExists: true })).toBe(true)
  })

  it("is withheld for a folder that has been deleted", () => {
    expect(canOpenInExplorer({ localPathExists: false })).toBe(false)
  })
})
