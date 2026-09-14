/**
 * Arciin brand lockups.
 *
 * Both marks are the reference app's own SVGs, copied into this repository so
 * it builds standalone. The wordmark composition - "Arciin" in Space Grotesk
 * bold with an accent-coloured full stop - is the same one the web app uses in
 * its sidebar and on its auth screens.
 */

import markUrl from "@/assets/brand/arciin-mark.svg"

/** Small dark lockup for the top-left of the onboarding shell. */
export function Wordmark() {
  return (
    <div className="shell__brand">
      <img className="shell__brand-mark" src={markUrl} alt="" aria-hidden />
      <span className="shell__wordmark">
        Arciin<span>.</span>
      </span>
    </div>
  )
}

/** Large white lockup for the orange hero panel. */
export function HeroWordmark() {
  return (
    <span className="hero__wordmark" aria-label="Arciin">
      Arciin<span aria-hidden>.</span>
    </span>
  )
}

/** The bare orange arch, used inside the search pulse. */
export function ArciinMark({ className }: { className?: string }) {
  return <img className={className} src={markUrl} alt="" aria-hidden />
}
