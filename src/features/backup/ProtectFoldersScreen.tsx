/**
 * "Protect folders from this PC" — the first-run backup offer.
 *
 * Shown only after the computer is paired *and* someone is signed in, because
 * authorizing backup needs a real user, not just a trusted device. It is an
 * offer, never a requirement: "Not now" is a first-class answer and leaves
 * Arciin Desktop working exactly as before.
 *
 * Two deliberate choices:
 *
 * - **Sizes stream in.** Measuring six trees can take a while, so the screen
 *   paints immediately and each row shows a skeleton until its own number
 *   arrives. Blocking the whole screen on the slowest folder would make it
 *   feel broken.
 * - **Rows are switches, not checkboxes.** They read ON / OFF, matching the
 *   language Arciin's own settings use, and the whole row is the hit target.
 * - **Known folders come first, picked folders after.** The six Windows
 *   folders are what most people mean, and "Add another folder" is the escape
 *   hatch for a project directory that lives somewhere else. Picking one opens
 *   the real Windows dialog; this screen never types or displays a path.
 */

import { useEffect, useMemo, useRef, useState } from "react"
import { FolderClosed, FolderPlus, HardDriveDownload, Loader2 } from "lucide-react"

import { FooterNote } from "@/components/footer-note"
import { ScrollHint, useScrollHint } from "@/components/scroll"
import { useVisibleRowCap } from "@/components/row-cap"
import { Button, LinkButton, Message } from "@/components/ui"
import * as ipc from "@/lib/ipc"
import { formatBytes, formatCount } from "@/lib/format"
import type { ActivationStage, AppError, BackupState, ProtectableFolder } from "@/types"
import { toAppError } from "@/types"

/** What each activation step is called on the button. */
function stageLabel(stage: ActivationStage | null): string {
  switch (stage?.state) {
    case "AUTHORIZING":
      return "Authorizing…"
    case "CREATING_PROFILE":
      return "Setting up…"
    case "CREATING_ROOTS":
      return "Adding folders…"
    case "SCANNING":
      // The long one. Named so a big folder reads as progress, not a hang.
      return "Scanning folders…"
    default:
      return "Starting backup…"
  }
}

type Measured = {
  fileCount: number
  totalBytes: number
  /** Still walking this folder. */
  pending: boolean
}

export function ProtectFoldersScreen({
  onDone,
  onSkip,
}: {
  onDone: () => void
  onSkip: () => void
}) {
  const [folders, setFolders] = useState<ProtectableFolder[] | null>(null)
  const [selected, setSelected] = useState<Set<string>>(new Set())
  const [sizes, setSizes] = useState<Record<string, Measured>>({})
  const [picking, setPicking] = useState(false)
  /**
   * Where activation has got to. `null` means it has not been started.
   *
   * This replaces a `starting` boolean. The boolean could not distinguish a
   * scan that legitimately takes minutes from a call that will never return,
   * so the screen sat on one spinner for both.
   */
  const [stage, setStage] = useState<ActivationStage | null>(null)
  const [progress, setProgress] = useState<BackupState | null>(null)
  const [error, setError] = useState<AppError | null>(null)
  const cancelled = useRef(false)
  const measuredCount = useMemo(
    () => Object.values(sizes).filter((size) => !size.pending).length,
    [sizes],
  )
  const { listRef, maxHeight } = useVisibleRowCap(folders?.length ?? 0, measuredCount)
  const listScroll = useScrollHint<HTMLUListElement>(folders?.length ?? 0)

  useEffect(() => {
    cancelled.current = false

    async function load() {
      try {
        const available = await ipc.backupFolders()
        if (cancelled.current) return
        setFolders(available)
        setSelected(new Set(available.filter((f) => f.selectedByDefault).map((f) => f.id)))
        setSizes(
          Object.fromEntries(
            available.map((f) => [f.id, { fileCount: 0, totalBytes: 0, pending: true }]),
          ),
        )

        // Sequential on purpose: six concurrent walks would thrash the disk
        // and finish later than doing them one at a time.
        for (const folder of available) {
          if (cancelled.current) return
          try {
            const summary = await ipc.backupMeasureFolder(folder.id)
            if (cancelled.current) return
            setSizes((current) => ({
              ...current,
              [folder.id]: {
                fileCount: summary.fileCount,
                totalBytes: summary.totalBytes,
                pending: false,
              },
            }))
          } catch {
            // One unmeasurable folder must not stop the rest; it simply shows
            // no number.
            setSizes((current) => ({
              ...current,
              [folder.id]: { fileCount: 0, totalBytes: 0, pending: false },
            }))
          }
        }
      } catch (err) {
        if (!cancelled.current) setError(toAppError(err))
      }
    }

    void load()
    return () => {
      cancelled.current = true
      void ipc.backupCancelMeasuring()
    }
  }, [])

  useEffect(() => {
    let stop: (() => void) | undefined
    void ipc.onBackupActivation(setStage).then((off) => {
      if (cancelled.current) off()
      else stop = off
    })
    return () => stop?.()
  }, [])

  // Once files are moving the queue counters are the honest progress report,
  // so they are polled only while that is true.
  useEffect(() => {
    if (stage?.state !== "UPLOADING" && stage?.state !== "ACTIVE") return
    let live = true
    const tick = async () => {
      try {
        const next = await ipc.backupState()
        if (live) setProgress(next)
      } catch {
        // A failed status read is not an activation failure.
      }
    }
    void tick()
    const timer = window.setInterval(() => void tick(), 1000)
    return () => {
      live = false
      window.clearInterval(timer)
    }
  }, [stage?.state])

  const estimate = useMemo(() => {
    let bytes = 0
    let files = 0
    let measuring = false
    for (const id of selected) {
      const size = sizes[id]
      if (!size) continue
      if (size.pending) measuring = true
      bytes += size.totalBytes
      files += size.fileCount
    }
    return { bytes, files, measuring }
  }, [selected, sizes])

  function toggle(id: string) {
    setSelected((current) => {
      const next = new Set(current)
      if (next.has(id)) next.delete(id)
      else next.add(id)
      return next
    })
  }

  /**
   * Open the Windows folder picker and add whatever comes back.
   *
   * Cancelling resolves to `null` and is a normal outcome, so it leaves the
   * screen exactly as it was. A folder that is already in the list comes back
   * as the same row rather than a duplicate, so this is also how re-picking
   * behaves.
   */
  async function addFolder() {
    if (picking || starting) return
    setPicking(true)
    setError(null)
    try {
      const folder = await ipc.backupPickFolder()
      if (!folder || cancelled.current) return

      setFolders((current) => {
        const existing = current ?? []
        return existing.some((f) => f.id === folder.id) ? existing : [...existing, folder]
      })
      setSelected((current) => new Set(current).add(folder.id))

      // Measured after it is on screen, so the row appears immediately and
      // fills in its size the way the known folders do.
      setSizes((current) =>
        current[folder.id] ? current : { ...current, [folder.id]: { fileCount: 0, totalBytes: 0, pending: true } },
      )
      try {
        const summary = await ipc.backupMeasureFolder(folder.id)
        if (cancelled.current) return
        setSizes((current) => ({
          ...current,
          [folder.id]: {
            fileCount: summary.fileCount,
            totalBytes: summary.totalBytes,
            pending: false,
          },
        }))
      } catch {
        setSizes((current) => ({
          ...current,
          [folder.id]: { fileCount: 0, totalBytes: 0, pending: false },
        }))
      }
    } catch (err) {
      if (!cancelled.current) setError(toAppError(err))
    } finally {
      if (!cancelled.current) setPicking(false)
    }
  }

  const starting = stage !== null && stage.state !== "ERROR" && stage.state !== "ACTIVE"

  /**
   * Turn backup on.
   *
   * The awaited call now returns once the server has the profile and roots —
   * a fraction of a second — and the rest reports itself through
   * `arciin://backup-activation`. So this resolves quickly even when the
   * folders are large, and the screen moves to real progress rather than
   * holding a spinner over the scan.
   */
  async function start() {
    if (selected.size === 0 || starting) return
    setStage({ state: "AUTHORIZING" })
    setError(null)
    try {
      await ipc.backupEnable([...selected])
    } catch (err) {
      // The native side reports its own failures through the stage event; this
      // catch covers the call itself failing.
      const failure = toAppError(err)
      setStage({ state: "ERROR", code: failure.code, message: failure.message })
    }
  }

  /** Put the screen back where a second attempt can be made. */
  function tryAgain() {
    setStage(null)
    setError(null)
  }

  // Activation failed. Say which step, and offer the way back — never leave
  // the screen sitting on a spinner for something that has already stopped.
  if (stage?.state === "ERROR") {
    return (
      <div className="stack">
        <div>
          <h1 className="title">Computer Backup couldn&rsquo;t start</h1>
          <p className="subtitle">{stage.message}</p>
        </div>
        <Message>{stage.message}</Message>
        <div className="stack stack--tight">
          <Button block onClick={tryAgain}>
            Try Again
          </Button>
          <div className="centered">
            <LinkButton onClick={onSkip}>Not now</LinkButton>
          </div>
        </div>
        <FooterNote>Nothing on this PC was changed or deleted.</FooterNote>
      </div>
    )
  }

  // Set up and running: show what it is actually doing rather than the setup
  // screen's spinner.
  if (stage && (stage.state === "UPLOADING" || stage.state === "ACTIVE")) {
    const status = progress?.status
    const protectedNames = progress?.roots.map((root) => root.displayName) ?? []
    return (
      <div className="stack">
        <div>
          <h1 className="title">Backing up this computer</h1>
          <p className="subtitle">
            {protectedNames.length > 0
              ? protectedNames.join(", ")
              : "Preparing your protected folders…"}
          </p>
        </div>
        <div className="card">
          {status ? (
            <BackupProgress
              filesSynced={status.filesSynced}
              filesOutstanding={status.filesOutstanding}
              bytesSynced={status.bytesSynced}
              paused={status.paused}
              onPause={() => void ipc.backupPause()}
              onResume={() => void ipc.backupResume()}
            />
          ) : (
            <p className="centered" style={{ margin: 0, fontSize: 13 }}>
              Starting…
            </p>
          )}
        </div>
        <div className="centered">
          <LinkButton onClick={onDone}>Done</LinkButton>
        </div>
        <FooterNote>Copied to your own server. Nothing is ever deleted from this PC.</FooterNote>
      </div>
    )
  }

  return (
    <div className="stack">
      <div>
        <h1 className="title">Protect folders from this PC</h1>
        <p className="subtitle">
          Keep important files backed up privately to your Arciin server. They
          stay exactly where they are in Windows.
        </p>
      </div>

      {folders === null ? (
        <ul className="folders" aria-busy>
          {[0, 1, 2, 3].map((row) => (
            <li key={row}>
              <div className="folder folder--loading" aria-hidden>
                <span className="folder__icon">
                  <FolderClosed size={16} />
                </span>
                <span className="folder__text">
                  <span className="skeleton" style={{ width: 74 }} />
                  <span className="skeleton" style={{ width: 122, height: 8 }} />
                </span>
              </div>
            </li>
          ))}
        </ul>
      ) : (
        <ul
          ref={(node) => {
            listRef.current = node
            listScroll.ref.current = node
          }}
          className={`folders${maxHeight ? " folders--scroll scroll-area" : ""}`}
          style={maxHeight ? ({ "--folders-max": maxHeight } as React.CSSProperties) : undefined}
        >
          {folders.map((folder) => {
            const size = sizes[folder.id]
            const on = selected.has(folder.id)
            return (
              <li key={folder.id}>
                <button
                  type="button"
                  className={`folder${on ? " folder--on" : ""}`}
                  onClick={() => toggle(folder.id)}
                  role="switch"
                  aria-checked={on}
                  disabled={starting}
                >
                  <span className="folder__icon" aria-hidden>
                    <FolderClosed size={16} />
                  </span>
                  <span className="folder__text">
                    <span className="folder__name">{folder.displayName}</span>
                    <span className="folder__meta">
                      {size?.pending ? (
                        <span className="skeleton" />
                      ) : size ? (
                        `${formatCount(size.fileCount)} files · ${formatBytes(size.totalBytes)}`
                      ) : (
                        "Unavailable"
                      )}
                    </span>
                  </span>
                  <span className={`switch${on ? " switch--on" : ""}`} aria-hidden>
                    <span className="switch__label">{on ? "ON" : "OFF"}</span>
                    <span className="switch__track">
                      <span className="switch__thumb" />
                    </span>
                  </span>
                </button>
              </li>
            )
          })}
        </ul>
      )}

      <ScrollHint visible={Boolean(maxHeight) && listScroll.hasMore} />

      {folders !== null ? (
        <div className="centered">
          <LinkButton onClick={() => void addFolder()} disabled={picking || starting}>
            <span className="row" style={{ gap: 6 }}>
              <FolderPlus size={13} aria-hidden />
              {picking ? "Choosing…" : "Add another folder"}
            </span>
          </LinkButton>
        </div>
      ) : null}

      {folders !== null ? (
        <div className="estimate">
          <span className="estimate__label">Estimated backup</span>
          <span className="estimate__value">
            {formatBytes(estimate.bytes)}
            <span className="estimate__pending">
              {" · "}
              {formatCount(estimate.files)} files
              {estimate.measuring ? " so far" : ""}
            </span>
          </span>
        </div>
      ) : null}

      {error ? <Message>{error.message}</Message> : null}

      <div className="stack stack--tight">
        <Button
          block
          onClick={() => void start()}
          loading={starting}
          disabled={selected.size === 0 || folders === null}
        >
          {starting ? null : <HardDriveDownload size={16} aria-hidden />}
          {starting ? stageLabel(stage) : "Start Backup"}
        </Button>
        <div className="centered">
          <LinkButton onClick={onSkip} disabled={starting}>
            Not now
          </LinkButton>
        </div>
      </div>

      <FooterNote>Copied to your own server. Nothing is ever deleted from this PC.</FooterNote>
    </div>
  )
}

/** Shown while the very first backup runs, and from Backup settings after. */
export function BackupProgress({
  filesSynced,
  filesOutstanding,
  bytesSynced,
  paused,
  onPause,
  onResume,
}: {
  filesSynced: number
  filesOutstanding: number
  bytesSynced: number
  paused: boolean
  onPause: () => void
  onResume: () => void
}) {
  const total = filesSynced + filesOutstanding
  // Guard the divide: a queue that has not been counted yet is 0/0.
  const percent = total > 0 ? Math.min(100, Math.round((filesSynced / total) * 100)) : 0

  return (
    <div className="progress">
      <div className="progress__row">
        <span>
          <span className="progress__count">{formatCount(filesSynced)}</span>
          {" / "}
          {formatCount(total)} files
        </span>
        <span>{formatBytes(bytesSynced)}</span>
      </div>
      <div
        className="progress__bar"
        role="progressbar"
        aria-valuenow={percent}
        aria-valuemin={0}
        aria-valuemax={100}
      >
        <div className="progress__fill" style={{ width: `${percent}%` }} />
      </div>
      <div className="progress__row">
        <span>
          {paused
            ? "Paused"
            : filesOutstanding > 0
              ? "You can keep using Arciin while this runs."
              : "Everything is backed up."}
        </span>
        {filesOutstanding > 0 || paused ? (
          <LinkButton onClick={paused ? onResume : onPause}>
            {paused ? "Resume" : "Pause"}
          </LinkButton>
        ) : null}
      </div>
    </div>
  )
}

/** The small health pill used by the status surface. */
export function HealthBadge({ health }: { health: string }) {
  const map: Record<string, { label: string; className: string }> = {
    UP_TO_DATE: { label: "Up to date", className: "health--upToDate" },
    SYNCING: { label: "Backing up", className: "health--syncing" },
    PAUSED: { label: "Paused", className: "health--paused" },
    OFFLINE: { label: "Server unavailable", className: "health--offline" },
    ERROR: { label: "Needs attention", className: "health--error" },
  }
  const entry = map[health] ?? map.OFFLINE

  return (
    <span className={`health ${entry.className}`}>
      {health === "SYNCING" ? (
        <Loader2 className="spin" size={11} aria-hidden />
      ) : (
        <span className="health__dot" aria-hidden />
      )}
      {entry.label}
    </span>
  )
}
