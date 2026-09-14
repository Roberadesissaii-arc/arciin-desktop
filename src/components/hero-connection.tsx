/**
 * The illustration in the hero panel.
 *
 * The web app's sign-in hero (`login-hero-showcase.tsx`) shows a miniature of
 * the dashboard, because signing in is what lands you there. This screen is the
 * step *before* sign-in, so a dashboard preview would promise the wrong thing.
 * What this screen is about is one computer becoming trusted by one server, so
 * that is what it draws.
 *
 * The composition follows the reference hero's own rule rather than sitting in
 * a box in the middle of the panel: it is oversized, anchored, bleeds off the
 * right edge, and fades out toward the bottom so it dissolves into the
 * headline instead of competing with it.
 *
 * Idiom is borrowed exactly — translucent white surfaces over the orange,
 * hairline borders, very small type, no colour of its own.
 *
 * Purely decorative. The whole panel is `aria-hidden` in the shell.
 */

import { Check, Fingerprint, Monitor } from "lucide-react"

import markUrl from "@/assets/brand/arciin-mark.svg"

/** Illustrative only — never real library counts. */
const SERVER_ROWS = [
  { label: "Inbox", width: "38%" },
  { label: "Files", width: "84%" },
  { label: "Images", width: "62%" },
  { label: "Videos", width: "51%" },
  { label: "Documents", width: "73%" },
] as const

export function HeroConnection({ deviceName }: { deviceName: string }) {
  return (
    <div className="scene">
      {/* This computer — the smaller end of the link. */}
      <div className="scene__device">
        <div className="scene__chrome" aria-hidden>
          <i />
          <i />
          <i />
        </div>
        <div className="scene__deviceBody">
          <span className="scene__icon">
            <Monitor size={16} strokeWidth={1.75} />
          </span>
          <div className="scene__text">
            <p className="scene__title">This PC</p>
            <p className="scene__sub">{deviceName || "Windows"}</p>
          </div>
        </div>

        <div className="scene__deviceFoot">
          <span className="scene__tag">
            <Fingerprint size={9} strokeWidth={2.25} aria-hidden />
            Windows
          </span>
          <span className="scene__tag">Desktop</span>
          <span className="scene__tagSpacer" aria-hidden />
          <span className="scene__tag scene__tag--quiet">v1</span>
        </div>
      </div>

      {/*
       * The wire. Dots travel from this computer toward the server, which is
       * the direction the pairing request actually goes. The path is inset
       * from the viewBox edges so it leaves the device card's bottom border
       * and meets the server card's top border, rather than running
       * underneath either of them.
       */}
      <svg
        className="scene__wire"
        viewBox="0 0 100 100"
        preserveAspectRatio="none"
        fill="none"
        aria-hidden
      >
        {/*
         * The viewBox is stretched to the panel width, so anything measured in
         * user units would be squashed with it. `non-scaling-stroke` keeps the
         * hairline and its dashes in screen units instead. The travellers are
         * zero-length paths with round caps for the same reason: a stroked dot
         * stays a circle under a non-uniform transform, where `<circle r>`
         * would flatten into an ellipse.
         */}
        <path
          id="arciin-wire"
          d="M13 2 C13 54, 74 44, 74 98"
          stroke="rgba(255,255,255,0.34)"
          strokeWidth="1.25"
          strokeDasharray="4 5"
          strokeLinecap="round"
          vectorEffect="non-scaling-stroke"
        />
        {[0, 0.85, 1.7].map((delay) => (
          <path
            key={delay}
            d="M0 0 l0.01 0"
            stroke="#ffffff"
            strokeWidth="5"
            strokeLinecap="round"
            vectorEffect="non-scaling-stroke"
            opacity="0"
          >
            <animateMotion dur="2.55s" repeatCount="indefinite" begin={`${delay}s`}>
              <mpath href="#arciin-wire" />
            </animateMotion>
            <animate
              attributeName="opacity"
              values="0;0.95;0.95;0"
              keyTimes="0;0.16;0.84;1"
              dur="2.55s"
              repeatCount="indefinite"
              begin={`${delay}s`}
            />
          </path>
        ))}
      </svg>

      {/* The server — larger, and deliberately cropped by the panel edge. */}
      <div className="scene__server">
        <span className="scene__rings" aria-hidden>
          <i />
          <i />
          <i />
        </span>

        <div className="scene__serverHead">
          <span className="scene__mark">
            <img src={markUrl} alt="" aria-hidden />
          </span>
          <div className="scene__text">
            <p className="scene__title">Your Arciin</p>
            <p className="scene__sub">Private server</p>
          </div>
          <span className="scene__status">
            <i aria-hidden />
            Online
          </span>
        </div>

        <ul className="scene__rows">
          {SERVER_ROWS.map((row) => (
            <li key={row.label} className="scene__row">
              <span className="scene__rowLabel">{row.label}</span>
              <span className="scene__rowBar" aria-hidden>
                <i style={{ width: row.width }} />
              </span>
            </li>
          ))}
        </ul>

        <div className="scene__trust">
          <span className="scene__check" aria-hidden>
            <Check size={9} strokeWidth={3} />
          </span>
          Paired once, then trusted
        </div>
      </div>
    </div>
  )
}
