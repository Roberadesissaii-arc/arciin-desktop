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

import { useEffect, useState } from "react"
import { KeyRound, Network, ShieldCheck } from "lucide-react"

import { HeroWordmark, Wordmark } from "@/components/brand"
import { ContextMenu } from "@/components/context-menu"
import { FooterNoteSlot } from "@/components/footer-note"
import { HeroConnection } from "@/components/hero-connection"
import { Status } from "@/components/ui"
import { ConnectingScreen } from "@/features/connection/ConnectingScreen"
import { DisconnectedScreen } from "@/features/connection/DisconnectedScreen"
import { ManualScreen } from "@/features/discovery/ManualScreen"
import { SearchScreen } from "@/features/discovery/SearchScreen"
import { PairingScreen } from "@/features/pairing/PairingScreen"
import * as ipc from "@/lib/ipc"
import { useOnboarding } from "@/stores/onboarding"

export function App() {
  const step = useOnboarding((state) => state.step)
  const boot = useOnboarding((state) => state.boot)
  const deviceName = useOnboarding((state) => state.deviceName)

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

  // The footer's middle slot. State rather than a plain ref, because screens
  // portal into it and must re-render once it exists.
  const [noteSlot, setNoteSlot] = useState<HTMLElement | null>(null)

  return (
    <FooterNoteSlot.Provider value={noteSlot}>
    {/* Mounted once, for every screen: it works off the document, not a
        subtree, so a right-click anywhere in this shell is covered. */}
    <ContextMenu />
    <div className="shell">
      <main className="shell__main">
        <Wordmark />
        <div className="shell__body">
          <div className="shell__content">
            <Step step={step} />
          </div>
        </div>
        {/*
          One row, three places. The middle one is filled by whichever screen
          is showing; see components/footer-note.
        */}
        <footer className="shell__footer">
          <span className="shell__footer__end">Copyright &copy; 2026 Arciin.</span>
          <span className="shell__footer__note" ref={setNoteSlot} />
          <span className="shell__footer__end shell__footer__end--right">
            Arciin Desktop {__APP_VERSION__}
          </span>
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
    </FooterNoteSlot.Provider>
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
    case "disconnected":
      return <DisconnectedScreen />
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
