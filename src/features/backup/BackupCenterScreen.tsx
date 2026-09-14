/**
 * Computer Backup — the management surface for a computer that is already
 * backing up.
 *
 * # Why this is a separate screen
 *
 * "Manage Backup" used to reopen the first-run wizard. For someone whose
 * backup has been running for weeks that reads as though their setup had been
 * lost, and it offers the one thing they do not want: setting it up again.
 * Setup and management are different jobs, so they are different screens, and
 * which one opens is decided from the real profile state.
 *
 * # Why it shows local paths
 *
 * "Which Desktop?" is a real question once folders can be redirected to
 * OneDrive or another drive, so each protected folder shows where it actually
 * is. That is safe *here* and only here: this is our own bundled UI in the
 * onboarding window. The path never goes to the server, and never to the
 * webview showing the server's page — that one has no IPC at all.
 */

import { useCallback, useEffect, useState } from "react"
import {
  FolderClosed,
  FolderOpen,
  FolderPlus,
  Pause,
  Play,
  ShieldCheck,
  X,
} from "lucide-react"

import { useVisibleRowCap } from "@/components/row-cap"
import { ScrollHint, useScrollHint } from "@/components/scroll"
import { Button, LinkButton, Message } from "@/components/ui"
import { HealthBadge } from "@/features/backup/ProtectFoldersScreen"
import * as ipc from "@/lib/ipc"
import { formatBytes, formatCount } from "@/lib/format"
import type {
  AppError,
  BackupState,
  ProtectableFolder,
  ProtectedRoot,
  ServerStorage,
} from "@/types"
import { toAppError } from "@/types"

/** How often the live counters refresh while this screen is open. */
const POLL_MS = 2000

export function BackupCenterScreen({
  deviceName,
  onClose,
  onStopped,
}: {
  deviceName: string
  onClose: () => void
  onStopped: () => void
}) {
  const [state, setState] = useState<BackupState | null>(null)
  const [error, setError] = useState<AppError | null>(null)
  const [busy, setBusy] = useState(false)
  const [confirmingStop, setConfirmingStop] = useState(false)
  const [removing, setRemoving] = useState<ProtectedRoot | null>(null)
  const [known, setKnown] = useState<ProtectableFolder[]>([])
  const [storage, setStorage] = useState<ServerStorage | null>(null)

  const refresh = useCallback(async () => {
    try {
      setState(await ipc.backupState())
    } catch (err) {
      setError(toAppError(err))
    }
  }, [])

  useEffect(() => {
    // Read once: disk capacity does not move fast enough to poll, and this
    // borrows the signed-in session to ask.
    void ipc.backupServerStorage().then(setStorage).catch(() => {})
  }, [])

  useEffect(() => {
    // The Windows folders that could still be added. Offered here so turning
    // on Documents later does not mean going back through first-run setup.
    void ipc.backupFolders().then(setKnown).catch(() => {})
  }, [])

  useEffect(() => {
    void ipc.sizeForBackupCenter()
    void refresh()
    // Polled rather than pushed: the counters change continuously while files
    // move, and an event per file would be far more traffic than a 2s read.
    const timer = window.setInterval(() => void refresh(), POLL_MS)
    return () => window.clearInterval(timer)
  }, [refresh])

  async function run(action: () => Promise<unknown>) {
    if (busy) return
    setBusy(true)
    setError(null)
    try {
      await action()
      await refresh()
    } catch (err) {
      setError(toAppError(err))
    } finally {
      setBusy(false)
    }
  }

  const status = state?.status
  const roots = (state?.roots ?? []).filter((root) => root.enabled)
  // A known folder already protected must not be offered a second time.
  const protectedKinds = new Set(roots.map((root) => root.kind))
  const available = known.filter((folder) => !protectedKinds.has(folder.kind))

  // Two lists, two boxes, scrolling independently on purpose. Sharing one
  // region meant reaching a folder in one list dragged the other along with
  // it. Each shows two rows; both are bounded, so the buttons below keep their
  // place however many folders exist.
  const protectedCap = useVisibleRowCap(roots.length, 0)
  const protectedScroll = useScrollHint<HTMLUListElement>(roots.length)
  const availableCap = useVisibleRowCap(available.length, 0)
  const availableScroll = useScrollHint<HTMLUListElement>(available.length)

  return (
    <div className="backup-center">
      <div className="row row--between">
        <div>
          <h1 className="title" style={{ textAlign: "left", fontSize: 21 }}>
            Computer Backup
          </h1>
          <p style={{ margin: "4px 0 0", fontSize: 12.5, color: "var(--text-secondary)" }}>
            {deviceName} &middot; backed up privately to your Arciin server
          </p>
        </div>
        {status ? <HealthBadge health={status.health} /> : null}
      </div>

      <StorageSummary storage={storage} status={status} />

      {status && status.filesFailed > 0 ? (
        <Message tone="info">
          {status.filesFailed} item{status.filesFailed === 1 ? "" : "s"} couldn&rsquo;t be
          backed up yet — usually a file that was open at the time. They&rsquo;ll be
          retried.
        </Message>
      ) : null}

      {status?.lastError ? <Message>{status.lastError}</Message> : null}
      {error ? <Message>{error.message}</Message> : null}

      <section>
        <div className="row row--between" style={{ marginBottom: 6 }}>
          <p className="section-label" style={{ margin: 0 }}>
            Protected folders
          </p>
          <LinkButton
            onClick={() =>
              void run(async () => {
                const folder = await ipc.backupPickFolder()
                // Only the new folder is sent: the server upserts roots by
                // their identifier and drops nothing, so the existing profile
                // and roots are reused rather than replaced.
                if (folder) await ipc.backupEnable([folder.id])
              })
            }
            disabled={busy}
          >
            <span className="row" style={{ gap: 6 }}>
              <FolderPlus size={13} aria-hidden />
              Add folder
            </span>
          </LinkButton>
        </div>

        <div className="listbox">
          <ul
            ref={(node) => {
              protectedCap.listRef.current = node
              protectedScroll.ref.current = node
            }}
            className="folders scroll-area listbox__list"
            style={
              protectedCap.maxHeight
                ? ({ "--folders-max": protectedCap.maxHeight } as React.CSSProperties)
                : undefined
            }
          >
          {roots.map((root) => (
            <li key={root.id}>
              <div className="folder folder--loading">
                <span className="folder__icon" aria-hidden>
                  <FolderClosed size={16} />
                </span>
                <span className="folder__text">
                  <span className="folder__name">{root.displayName}</span>
                  {/* Local, and staying local. */}
                  <span className="folder__path" title={root.localPath}>
                    {root.localPath}
                  </span>
                  <span className="folder__meta">
                    {formatCount(root.fileCount)} files &middot;{" "}
                    {formatBytes(root.bytesSynced)}
                    {root.pending > 0 ? ` · ${formatCount(root.pending)} pending` : ""}
                    {root.failed > 0 ? ` · ${formatCount(root.failed)} failed` : ""}
                  </span>
                </span>
                <span className="row" style={{ gap: 4 }}>
                  <LinkButton
                    onClick={() => void ipc.backupOpenRoot(root.id)}
                    title="Open this folder in File Explorer"
                  >
                    <FolderOpen size={13} aria-hidden />
                  </LinkButton>
                  <LinkButton
                    onClick={() => setRemoving(root)}
                    title="Stop backing up this folder"
                    disabled={busy}
                  >
                    <X size={13} aria-hidden />
                  </LinkButton>
                </span>
              </div>
            </li>
          ))}
          </ul>
          <ScrollHint visible={protectedScroll.hasMore} floating />
        </div>

        {roots.length === 0 ? (
          <p className="centered" style={{ fontSize: 12.5, color: "var(--text-secondary)" }}>
            No folders are being backed up from this computer.
          </p>
        ) : null}
      </section>

      {available.length > 0 ? (
        <section>
          <p className="section-label">Also available on this PC</p>
          {/*
            Its own box with its own scroll, independent of the list above.
            One region for both meant scrolling to reach a folder here also
            moved the protected list, which is not what either list is for.
          */}
          <div className="listbox">
            <ul
              ref={(node) => {
                availableCap.listRef.current = node
                availableScroll.ref.current = node
              }}
              className="folders scroll-area listbox__list"
              style={
                availableCap.maxHeight
                  ? ({ "--folders-max": availableCap.maxHeight } as React.CSSProperties)
                  : undefined
              }
            >
              {available.map((folder) => (
                <li key={folder.id}>
                  <button
                    type="button"
                    className="folder"
                    disabled={busy}
                    onClick={() => void run(() => ipc.backupEnable([folder.id]))}
                  >
                    <span className="folder__icon" aria-hidden>
                      <FolderClosed size={16} />
                    </span>
                    <span className="folder__text">
                      <span className="folder__name">{folder.displayName}</span>
                      <span className="folder__meta">Not backed up</span>
                    </span>
                    <span className="btn-link btn-link--accent" aria-hidden>
                      Add
                    </span>
                  </button>
                </li>
              ))}
            </ul>
            <ScrollHint visible={availableScroll.hasMore} floating />
          </div>
        </section>
      ) : null}

      {removing ? (
        <div className="card stack stack--tight">
          <p style={{ margin: 0, fontSize: 12.5, lineHeight: 1.6, color: "var(--text-secondary)" }}>
            Stop backing up <strong>{removing.displayName}</strong>? Files already on
            your Arciin server stay there, and{" "}
            <strong>nothing on this PC is deleted</strong>. Other folders keep backing
            up.
          </p>
          <div className="row" style={{ gap: 8 }}>
            <Button compact variant="secondary" onClick={() => setRemoving(null)}>
              Cancel
            </Button>
            <Button
              compact
              onClick={() =>
                void run(async () => {
                  await ipc.backupRemoveRoot(removing.id)
                  setRemoving(null)
                })
              }
            >
              Stop backing it up
            </Button>
          </div>
        </div>
      ) : null}

      <div className="row" style={{ gap: 8 }}>
        {status?.paused ? (
          <Button compact variant="secondary" onClick={() => void run(ipc.backupResume)}>
            <Play size={14} aria-hidden />
            Resume Backup
          </Button>
        ) : (
          <Button compact variant="secondary" onClick={() => void run(ipc.backupPause)}>
            <Pause size={14} aria-hidden />
            Pause Backup
          </Button>
        )}
        <Button compact onClick={onClose}>
          Done
        </Button>
      </div>

      {/*
        Deliberately last, quiet, and worded so it cannot be confused with
        disconnecting the computer — which is a Device-trust action and lives
        in Settings -> Devices, not here.
      */}
      <div className="card stack stack--tight">
        {confirmingStop ? (
          <>
            <p style={{ margin: 0, fontSize: 12.5, lineHeight: 1.6, color: "var(--text-secondary)" }}>
              Stop Computer Backup on this PC? Files already on your Arciin server
              stay there, and <strong>nothing on this PC is deleted</strong>. This
              computer <strong>stays connected and paired</strong> — only the
              background backup stops.
            </p>
            <div className="row" style={{ gap: 8 }}>
              <Button compact variant="secondary" onClick={() => setConfirmingStop(false)}>
                Cancel
              </Button>
              <Button
                compact
                onClick={() =>
                  void run(async () => {
                    await ipc.backupForget()
                    setConfirmingStop(false)
                    onStopped()
                  })
                }
              >
                Stop Computer Backup
              </Button>
            </div>
          </>
        ) : (
          <div className="row row--between">
            <span style={{ fontSize: 12.5, color: "var(--text-secondary)" }}>
              Stop backing up this computer
            </span>
            <LinkButton onClick={() => setConfirmingStop(true)}>
              Stop Computer Backup
            </LinkButton>
          </div>
        )}
      </div>

      <p className="privacy-note">
        <ShieldCheck size={14} aria-hidden />
        Copied to your own server. Nothing is ever deleted from this PC.
      </p>
    </div>
  )
}

/**
 * Where the bytes are: what the server holds, and what this computer still has
 * to send.
 *
 * Two different questions, so two lines. "Will it fit?" is answered by the
 * server's own disk, which only it can report; "how much is left?" is answered
 * by the local queue. Conflating them would be worse than showing neither.
 *
 * Anything the server could not probe is shown as unknown rather than guessed:
 * a made-up capacity is how someone ends up filling a disk.
 */
function StorageSummary({
  storage,
  status,
}: {
  storage: ServerStorage | null
  status?: BackupState["status"]
}) {
  const used = storage?.usageBytes ?? null
  const total = storage?.totalBytes ?? null
  const free = storage?.availableBytes ?? null
  const outstanding = status?.bytesOutstanding ?? 0

  // Only meaningful when the server could read the volume.
  const percent =
    used !== null && total !== null && total > 0
      ? Math.min(100, Math.round((used / total) * 100))
      : null

  // The honest warning: more to upload than the disk has room for.
  const wontFit = free !== null && outstanding > free

  return (
    <div className="storage">
      <div className="storage__row">
        <span className="storage__label">Arciin storage</span>
        <span className="storage__value">
          {used !== null && total !== null
            ? `${formatBytes(used)} of ${formatBytes(total)}`
            : "Unknown"}
        </span>
      </div>

      {percent !== null ? (
        <div
          className="progress__bar"
          role="progressbar"
          aria-valuenow={percent}
          aria-valuemin={0}
          aria-valuemax={100}
        >
          <div className="progress__fill" style={{ width: `${percent}%` }} />
        </div>
      ) : null}

      <div className="storage__row">
        <span className="storage__hint">
          {free !== null ? `${formatBytes(free)} free` : "Free space unknown"}
        </span>
        <span className={`storage__hint${wontFit ? " storage__hint--warn" : ""}`}>
          {outstanding > 0
            ? `${formatBytes(outstanding)} to upload`
            : `${formatBytes(status?.bytesSynced ?? 0)} backed up`}
        </span>
      </div>

      {wontFit ? (
        <Message>
          This computer has more to upload than your Arciin server has free
          space. Free some space on the server, or protect fewer folders.
        </Message>
      ) : null}
    </div>
  )
}
