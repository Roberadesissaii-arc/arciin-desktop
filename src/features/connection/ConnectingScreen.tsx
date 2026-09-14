/**
 * The wait between "paired" and "Arciin is on screen".
 *
 * Each phase is named rather than hidden behind one spinner, because the steps
 * genuinely differ - confirming the server is who it claims to be, proving this
 * device, securing the session, opening the window - and when something fails,
 * knowing which one it was is most of the diagnosis.
 */

import { Check, Loader2 } from "lucide-react"

import { ArciinMark } from "@/components/brand"
import type { ConnectPhase } from "@/stores/onboarding"
import { useOnboarding } from "@/stores/onboarding"

const PHASES: Array<{ id: ConnectPhase; label: string }> = [
  { id: "verifying", label: "Checking the server" },
  { id: "authorizing", label: "Authorising this computer" },
  { id: "securing", label: "Securing the connection" },
  { id: "opening", label: "Opening Arciin" },
]

export function ConnectingScreen() {
  const phase = useOnboarding((state) => state.connectPhase)
  const target = useOnboarding((state) => state.target)
  const current = PHASES.findIndex((entry) => entry.id === phase)

  return (
    <div className="stack" style={{ alignItems: "center", gap: 24 }}>
      <div className="radar">
        <span className="radar__ring" />
        <span className="radar__ring" />
        <span className="radar__ring" />
        <ArciinMark className="radar__mark" />
      </div>

      <div>
        <h1 className="title">{target ? `Connecting to ${target.name}` : "Connecting"}</h1>
        <p className="subtitle">This only takes a moment.</p>
      </div>

      <ul
        style={{
          listStyle: "none",
          margin: 0,
          padding: 0,
          display: "flex",
          flexDirection: "column",
          gap: 10,
          minWidth: 240,
        }}
      >
        {PHASES.map((entry, index) => {
          const done = index < current
          const active = index === current
          return (
            <li
              key={entry.id}
              className="row"
              style={{
                gap: 10,
                fontSize: 13,
                color: done
                  ? "var(--text-secondary)"
                  : active
                    ? "var(--text-strong)"
                    : "var(--text-faint)",
              }}
            >
              {done ? (
                <Check size={15} aria-hidden style={{ color: "var(--arciin-accent)" }} />
              ) : active ? (
                <Loader2
                  size={15}
                  className="spin"
                  aria-hidden
                  style={{ color: "var(--arciin-accent)" }}
                />
              ) : (
                <span
                  aria-hidden
                  style={{
                    width: 15,
                    height: 15,
                    display: "inline-block",
                  }}
                />
              )}
              {entry.label}
            </li>
          )
        })}
      </ul>
    </div>
  )
}
