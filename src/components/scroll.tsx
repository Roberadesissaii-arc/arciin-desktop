/**
 * Scrolling regions, without a scrollbar.
 *
 * # The rule
 *
 * This app never shows a native scrollbar. A scroll track running down the
 * side of a small settings window is chrome the user did not ask for, it
 * changes width between machines, and it makes a compact panel look like a
 * document. Scrolling still works exactly as normal — wheel, trackpad, keys,
 * touch — it just is not drawn.
 *
 * What replaces it is a single down chevron at the foot of the region,
 * meaning only "there is more below". It appears when there is, disappears at
 * the end, and costs no horizontal space.
 *
 * The bar is hidden in CSS (`.scroll-area`); this module supplies the hint.
 */

import { useCallback, useEffect, useRef, useState } from "react"
import { ChevronDown } from "lucide-react"

/**
 * Track whether a scrolling element still has content below the fold.
 *
 * Re-measured on scroll, on resize, and whenever the content changes, because
 * a list that grows when a folder is added must gain the hint without the
 * person touching anything. A few pixels of slack stops the chevron flickering
 * at the very bottom on fractional scroll positions.
 */
export function useScrollHint<T extends HTMLElement>(revision: unknown = null) {
  const ref = useRef<T>(null)
  const [hasMore, setHasMore] = useState(false)

  const measure = useCallback(() => {
    const element = ref.current
    if (!element) return
    const remaining = element.scrollHeight - element.clientHeight - element.scrollTop
    setHasMore(remaining > 4)
  }, [])

  useEffect(() => {
    const element = ref.current
    // The list often does not exist on first render — it waits for the folders
    // to load. Without `revision` in the deps this effect ran once against a
    // null ref and never attached, so the chevron never appeared no matter how
    // much was below the fold.
    if (!element) return

    measure()
    element.addEventListener("scroll", measure, { passive: true })

    // Catches the list growing, the window resizing, and fonts settling —
    // all of which change whether there is anything below.
    const observer = new ResizeObserver(measure)
    observer.observe(element)
    for (const child of Array.from(element.children)) observer.observe(child)

    return () => {
      element.removeEventListener("scroll", measure)
      observer.disconnect()
    }
  }, [measure, revision])

  return { ref, hasMore, measure }
}

/**
 * The "there is more below" chevron. Nothing else; it is not a control.
 *
 * `floating` lifts it out of the flow to sit over the foot of the list. In the
 * flow it would claim its own strip of height, which both looks like a second
 * container and steals space from the rows.
 */
export function ScrollHint({ visible, floating }: { visible: boolean; floating?: boolean }) {
  return (
    <div
      className={`scroll-hint${visible ? " scroll-hint--on" : ""}${floating ? " scroll-hint--float" : ""}`}
      aria-hidden
    >
      <ChevronDown size={16} />
    </div>
  )
}
