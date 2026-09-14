/**
 * The onboarding shell.
 *
 * Left column: brand, the current step, legal footer. Right column: the orange
 * hero panel. The composition is the reference app's `/login` page, so the
 * desktop's first screen and the server's own sign-in screen are continuous.
 *
 * This shell covers connection only. Once a server is open, everything the
 * user sees is served by that server - there is no copy of Dashboard, Files or
 * Settings in this application.
 */

import { useEffect } from "react"
import { KeyRound, Network, ShieldCheck } from "lucide-react"

import { HeroWordmark, Wordmark } from "@/components/brand"
import { HeroConnection } from "@/components/hero-connection"
import { Status } from "@/components/ui"
import { ConnectingScreen } from "@/features/connection/ConnectingScreen"
import { DisconnectedScreen } from "@/features/connection/DisconnectedScreen"
import { ManualScreen } from "@/features/discovery/ManualScreen"
import { SearchScreen } from "@/features/discovery/SearchScreen"
import { PairingScreen } from "@/features/pairing/PairingScreen"
import { BackupSettingsScreen } from "@/features/backup/BackupSettingsScreen"
import { BackupCenterScreen } from "@/features/backup/BackupCenterScreen"
import { ProtectFoldersScreen } from "@/features/backup/ProtectFoldersScreen"
import * as ipc from "@/lib/ipc"
import { useOnboarding } from "@/stores/onboarding"

export function App() {
  const step = useOnboarding((state) => state.step)
  const boot = useOnboarding((state) => state.boot)
  const deviceName = useOnboarding((state) => state.deviceName)
  // The Backup Center lays itself out to the window and scrolls exactly one
  // list inside itself, so the page must not scroll as well. Everything else
  // is a short centred form that can scroll as a whole.
  const fills = step === "backupCenter"

  useEffect(() => {
    void boot()
  }, [boot])

  useEffect(() => {
    // Registered once for the life of the window: a disconnect can arrive at
    // any moment, including while the Arciin window is in front.
    let dispose: (() => void) | undefined
    void ipc.onDeviceRevoked((serverId) => {
      void useOnboarding.getState().deviceRevoked(serverId)
    }).then((unlisten) => {
      dispose = unlisten
    })
    return () => dispose?.()
  }, [])

  useEffect(() => {
    // The one action the Arciin page may ask of us. Verified natively before
    // it ever reaches here.
    let dispose: (() => void) | undefined
    void ipc.onOpenBackupSetup(() => {
      void useOnboarding.getState().openBackupSetup()
    }).then((unlisten) => {
      dispose = unlisten
    })
    return () => dispose?.()
  }, [])

  return (
    <div className="shell">
      <main className="shell__main">
        <Wordmark />
        <div className={`shell__body${fills ? " shell__body--fill" : ""}`}>
          <div className={`shell__content${fills ? " shell__content--fill" : ""}`}>
            <Step step={step} />
          </div>
        </div>
        <footer className="shell__footer">
          <span>Copyright &copy; 2026 Arciin.</span>
          <span>Arciin Desktop {__APP_VERSION__}</span>
        </footer>
      </main>

      <aside className="shell__hero" aria-hidden>
        <div className="hero">
          <div className="hero__top">
            <HeroWordmark />
            <span className="hero__badge">Desktop</span>
          </div>
          <div className="hero__middle">
            <HeroConnection deviceName={deviceName} />
          </div>
          <div className="hero__bottom">
            <h2 className="hero__headline">Your server,
              <br />
              your control.</h2>
            <p className="hero__sub">
              Arciin Desktop connects this computer to your own Arciin server.
              Nothing is stored anywhere else.
            </p>
            <ul className="hero__points">
              <li className="hero__point">
                <Network size={15} />
                Finds your server on this network
              </li>
              <li className="hero__point">
                <KeyRound size={15} />
                Pairs once with a code you generate
              </li>
              <li className="hero__point">
                <ShieldCheck size={15} />
                Keeps its credential in Windows
              </li>
            </ul>
          </div>
        </div>
      </aside>
    </div>
  )
}

function Step({ step }: { step: ReturnType<typeof useOnboarding.getState>["step"] }) {
  switch (step) {
    case "booting":
      return <Status>Starting Arciin Desktop&hellip;</Status>
    case "manual":
      return <ManualScreen />
    case "pairing":
      return <PairingScreen />
    case "connecting":
      return <ConnectingScreen />
    case "backupSettings":
      return <BackupSettings />
    case "disconnected":
      return <DisconnectedScreen />
    case "protectFolders":
      return <ProtectFolders />
    case "backupCenter":
      return <BackupCenter />
    case "connected":
      // The Arciin window is in front; this one is hidden behind it. Keeping a
      // sane state here matters for when the user returns after a revocation.
      return <Status>Arciin is open.</Status>
    case "searching":
    case "results":
    default:
      return <SearchScreen />
  }
}

/** The backup offer. Both exits return to the running Arciin window. */
function ProtectFolders() {
  const close = useOnboarding((state) => state.closeBackupUi)
  return <ProtectFoldersScreen onDone={close} onSkip={close} />
}

/** Management for a computer that is already backing up. */
function BackupCenter() {
  const close = useOnboarding((state) => state.closeBackupUi)
  const deviceName = useOnboarding((state) => state.deviceName)
  return (
    <BackupCenterScreen
      deviceName={deviceName}
      onClose={close}
      // Backup stopped: there is nothing left to manage, so the next visit
      // should offer setup again rather than an empty Backup Center.
      onStopped={close}
    />
  )
}

/** The native backup status screen, with its way back. */
function BackupSettings() {
  const skipBackup = useOnboarding((state) => state.skipBackup)
  return <BackupSettingsScreen onBack={skipBackup} />
}
