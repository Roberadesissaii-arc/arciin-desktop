/**
 * The reassurance line, rendered into the window's own footer.
 *
 * # Why it is not simply the last element on the screen
 *
 * It used to be, and it sat in whichever screen was showing — which put it
 * *inside* the bounded, scrolling content area. Two things followed. It was the
 * last child of a container with a fixed height, so when the content grew it
 * was the part that got clipped; and it drew a third line of text immediately
 * above the footer's own two, so the bottom of the window had the copyright on
 * the left, the version on the right, and this floating between them on a line
 * of its own, aligned with neither.
 *
 * The footer is outside the scrolling area and already spans the width, so the
 * line belongs there: copyright left, this centred, version right, one row.
 *
 * # Why a portal
 *
 * The three pieces share a row but not an owner. The copyright and the version
 * belong to the shell; the sentence changes per screen — "nothing was changed
 * or deleted" after a failure reads very differently from "copied to your own
 * server" while it runs — so the words have to stay with the screen that means
 * them. A portal puts them in the footer's middle slot without moving the copy
 * away from its context, and without the shell having to know every screen.
 *
 * Only one screen renders at a time, and within a screen these live in
 * mutually exclusive branches, so the slot never has to arbitrate between two.
 */

import { createContext, useContext, type ReactNode } from "react"
import { createPortal } from "react-dom"
import { ShieldCheck } from "lucide-react"

/** The footer element screens render their note into, once the shell has one. */
export const FooterNoteSlot = createContext<HTMLElement | null>(null)

export function FooterNote({ children }: { children: ReactNode }) {
  const slot = useContext(FooterNoteSlot)
  // Null on the very first render, before the footer's ref callback has run.
  // Nothing is lost: setting the ref re-renders, and this lands on the pass
  // straight after.
  if (!slot) return null

  return createPortal(
    <span className="privacy-note">
      <ShieldCheck size={13} aria-hidden />
      {children}
    </span>,
    slot,
  )
}
