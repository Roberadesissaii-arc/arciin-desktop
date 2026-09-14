/**
 * Computer Backup status and settings.
 *
 * Deliberately small. The server's own web UI already has Computers and
 * Settings → Devices, and wrapping that in a second navigation system would
 * mean maintaining two products. This screen covers only what the web app
 * cannot: what the *local* engine is doing right now, and the two controls
 * that only make sense on this machine — pause, and stop backing up.
 */

import { useCallback, useEffect, useState } from "react"
import { ArrowLeft, FolderClosed, FolderPlus, HardDrive } from "lucide-react"

import { Button, LinkButton, Message } from "@/components/ui"
import { BackupProgress, HealthBadge } from "@/features/backup/ProtectFoldersScreen"
import * as ipc from "@/lib/ipc"
import type { AppError, BackupState } from "@/types"
import { toAppError } from "@/types"

/** How often the status refreshes while the screen is open. */
const POLL_MS = 2000

export function BackupSettingsScreen({ onBack }: { onBack: () => void }) {
  const [state, setState] = useState<BackupState | null>(null)
  const [error, setError] = useState<AppError | null>(null)
  const [confirmingStop, setConfirmingStop] = useState(false)
  const [adding, setAdding] = useState(false)

  const refresh = useCallback(async () => {
    try {
      setState(await ipc.backupState())
    } catch (err) {
      setError(toAppError(err))
    }
  }, [])

  useEffect(() => {
    void refresh()
    // Polled rather than pushed: the engine's progress changes continuously,
    // and an event per file would be far more traffic than a 2s read.
    const timer = window.setInterval(() => void refresh(), POLL_MS)
    return () => window.clearInterval(timer)
  }, [refresh])

  async function run(action: () => Promise<void>) {
    setError(null)
    try {
      await action()
      await refresh()
    } catch (err) {
      setError(toAppError(err))
    }
  }

  /**
   * Protect one more folder without turning backup off first.
   *
   * Only the new folder is sent. The server upserts roots by
   * `sourcePathIdentifier` and does not drop the ones left out, so the
   * existing roots are untouched and no second profile is created.
   *
   * Re-sending the existing roots would be wrong as well as unnecessary: a
   * custom root's kind is the literal `CUSTOM`, which is not a selection id,
   * so they would resolve to nothing on the way back in.
   */
  async function addFolder() {
    if (adding) return
    setAdding(true)
    setError(null)
    try {
      const folder = await ipc.backupPickFolder()
      if (folder) {
        await ipc.backupEnable([folder.id])
        await refresh()
      }
    } catch (err) {
      setError(toAppError(err))
    } finally {
      setAdding(false)
    }
  }

  const status = state?.status

  return (
    <div className="stack">
      <div>
        <h1 className="title">Computer Backup</h1>
        {status ? (
          <p className="centered" style={{ margin: "10px 0 0" }}>
            <HealthBadge health={status.health} />
          </p>
        ) : null}
      </div>

      {state && !state.enabled ? (
        <div className="card centered">
          <p style={{ margin: 0, fontSize: 13, color: "var(--text-secondary)" }}>
            Backup isn&rsquo;t set up on this computer.
          </p>
        </div>
      ) : null}

      {status ? (
        <div className="card">
          <BackupProgress
            filesSynced={status.filesSynced}
            filesOutstanding={status.filesOutstanding}
            bytesSynced={status.bytesSynced}
            paused={status.paused}
            onPause={() => void run(ipc.backupPause)}
            onResume={() => void run(ipc.backupResume)}
          />
        </div>
      ) : null}

      {status && status.filesFailed > 0 ? (
        <Message tone="info">
          {status.filesFailed} item{status.filesFailed === 1 ? "" : "s"} couldn&rsquo;t
          be backed up yet — usually a file that was open at the time. They&rsquo;ll
          be retried.
        </Message>
      ) : null}

      {status?.lastError ? <Message>{status.lastError}</Message> : null}

      {state && state.roots.length > 0 ? (
        <section>
          <div className="row row--between" style={{ marginBottom: 6 }}>
            <p className="section-label" style={{ margin: 0 }}>
              Protected folders
            </p>
            {/*
              The same picker the first-run screen uses. Protecting one more
              folder is the commonest reason to come back here, and it must not
              mean turning backup off and setting it up again.
            */}
            <LinkButton onClick={() => void addFolder()} disabled={adding}>
              <span className="row" style={{ gap: 6 }}>
                <FolderPlus size={13} aria-hidden />
                {adding ? "Choosing…" : "Add folder"}
              </span>
            </LinkButton>
          </div>
          <ul className="folders">
            {state.roots.map((root) => (
              <li key={root.id}>
                <div className="folder folder--loading">
                  <span className="folder__icon" aria-hidden>
                    <FolderClosed size={16} />
                  </span>
                  <span className="folder__text">
                    <span className="folder__name">{root.displayName}</span>
                    <span className="folder__meta">
                      {root.enabled ? "Protected" : "Not protected"}
                    </span>
                  </span>
                </div>
              </li>
            ))}
          </ul>
        </section>
      ) : null}

      {error ? <Message>{error.message}</Message> : null}

      {state?.enabled ? (
        <div className="card stack stack--tight">
          {confirmingStop ? (
            <>
              <p style={{ margin: 0, fontSize: 12.5, lineHeight: 1.6, color: "var(--text-secondary)" }}>
                Stop backing up this computer? Files already on your Arciin
                server stay there, and <strong>nothing on this PC is deleted</strong>.
                This computer stays paired.
              </p>
              <div className="row" style={{ gap: 8 }}>
                <Button
                  compact
                  variant="secondary"
                  onClick={() => setConfirmingStop(false)}
                >
                  Cancel
                </Button>
                <Button
                  compact
                  onClick={() =>
                    void run(async () => {
                      await ipc.backupForget()
                      setConfirmingStop(false)
                    })
                  }
                >
                  Stop Backup
                </Button>
              </div>
            </>
          ) : (
            <div className="row row--between">
              <span style={{ fontSize: 12.5, color: "var(--text-secondary)" }}>
                <HardDrive size={13} aria-hidden style={{ verticalAlign: -2, marginRight: 6 }} />
                Backing up to your Arciin server
              </span>
              <LinkButton onClick={() => setConfirmingStop(true)}>Stop backup</LinkButton>
            </div>
          )}
        </div>
      ) : null}

      <div className="centered">
        <LinkButton onClick={onBack}>
          <span className="row" style={{ gap: 6 }}>
            <ArrowLeft size={13} aria-hidden />
            Back
          </span>
        </LinkButton>
      </div>
    </div>
  )
}
