/**
 * Capping a list to a fixed number of rows.
 *
 * Shared so every list in the app agrees on how many rows are on screen and
 * how that height is worked out.
 */

import { useLayoutEffect, useRef, useState } from "react"

/**
 * How many folder rows stay on screen at once.
 *
 * The list is the only part of this screen that grows — a person can keep
 * adding folders — so it is the only part allowed to scroll. Capping it here
 * keeps the heading, the running total and Start Backup where they were,
 * instead of the whole screen turning into a scrolling page.
 */
const DEFAULT_VISIBLE = 2

/**
 * Cap a list to exactly `visible` rows, measured from the rows
 * themselves.
 *
 * Measured rather than a fixed pixel height on purpose: a row's height depends
 * on the font Windows actually renders, the display scale, and whether a name
 * wraps. A hard-coded value would be right on one machine and show three and a
 * sliver on the next. Returns `undefined` while there is nothing to cap, which
 * leaves a short list unscrolled and with no scrollbar.
 */
export function useVisibleRowCap(rowCount: number, revision: number, visible = DEFAULT_VISIBLE) {
  const listRef = useRef<HTMLUListElement>(null)
  const [maxHeight, setMaxHeight] = useState<string>()

  useLayoutEffect(() => {
    const list = listRef.current
    if (!list || rowCount <= visible) {
      setMaxHeight(undefined)
      return
    }

    const rows = Array.from(list.children).slice(0, visible)
    if (rows.length < visible) return

    const gap = Number.parseFloat(getComputedStyle(list).rowGap) || 0
    const total =
      rows.reduce((sum, row) => sum + row.getBoundingClientRect().height, 0) +
      gap * (visible - 1)

    setMaxHeight(`${Math.round(total)}px`)
    // `revision` re-measures as each folder's size lands: a row swaps a
    // loading skeleton for real text, which can change its height slightly,
    // and a cap measured from the skeleton would then show a sliver of a
    // fifth row.
  }, [rowCount, revision, visible])

  return { listRef, maxHeight }
}

