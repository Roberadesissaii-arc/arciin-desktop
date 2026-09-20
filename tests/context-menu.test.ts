/**
 * What the right-click menu offers, and what it refuses to offer.
 *
 * WebView2's own menu is what appears without this: Back, Refresh, Save as,
 * Print, More tools — browser chrome in a product window, with no clipboard
 * commands on a text field, which is why right-click read as doing nothing.
 *
 * These cover the rules rather than the rendering: a menu that offers Cut with
 * nothing selected, or Select all on an empty box, is worse than no menu,
 * because every disabled-looking item still has to be read before it is
 * dismissed.
 */

import { describe, expect, it } from "vitest"

import { menuItemsFor } from "@/components/context-menu"

const NOTHING_SELECTED = { field: null, documentSelection: "" }

function commands(items: ReturnType<typeof menuItemsFor>): string[] {
  return (items ?? []).map((item) => item.command)
}

function enabled(items: ReturnType<typeof menuItemsFor>, command: string): boolean {
  return (items ?? []).find((item) => item.command === command)?.enabled ?? false
}

describe("right-clicking ordinary chrome", () => {
  it("shows no menu at all", () => {
    // Not an empty menu: the browser menu is suppressed either way, and an
    // empty panel appearing under the pointer is its own kind of broken.
    expect(menuItemsFor(NOTHING_SELECTED)).toBeNull()
  })

  it("ignores a selection that is only whitespace", () => {
    expect(menuItemsFor({ field: null, documentSelection: "   \n\t " })).toBeNull()
  })
})

describe("right-clicking selected text outside a field", () => {
  it("offers copy, and only copy", () => {
    const items = menuItemsFor({ field: null, documentSelection: "arciin" })
    expect(commands(items)).toEqual(["copy"])
    expect(enabled(items, "copy")).toBe(true)
  })
})

describe("right-clicking a text field", () => {
  const empty = { field: { length: 0, hasSelection: false }, documentSelection: "" }
  const typed = { field: { length: 12, hasSelection: false }, documentSelection: "" }
  const selected = { field: { length: 12, hasSelection: true }, documentSelection: "" }

  it("offers the four clipboard commands in the order people expect", () => {
    expect(commands(menuItemsFor(typed))).toEqual(["cut", "copy", "paste", "selectAll"])
  })

  it("always allows paste, because the field is the reason to right-click it", () => {
    // The pairing code and the manual server address are both pasted, and
    // before this there was no way to do it with the mouse.
    for (const target of [empty, typed, selected]) {
      expect(enabled(menuItemsFor(target), "paste")).toBe(true)
    }
  })

  it("refuses cut and copy when nothing is selected", () => {
    expect(enabled(menuItemsFor(typed), "cut")).toBe(false)
    expect(enabled(menuItemsFor(typed), "copy")).toBe(false)
  })

  it("allows cut and copy once there is a selection", () => {
    expect(enabled(menuItemsFor(selected), "cut")).toBe(true)
    expect(enabled(menuItemsFor(selected), "copy")).toBe(true)
  })

  it("refuses select all on an empty box", () => {
    expect(enabled(menuItemsFor(empty), "selectAll")).toBe(false)
  })

  it("allows select all once something is typed", () => {
    expect(enabled(menuItemsFor(typed), "selectAll")).toBe(true)
  })

  it("prefers the field over a stale selection elsewhere on the page", () => {
    // Right-clicking a box while text is selected somewhere else must act on
    // the box, not offer to copy the other thing.
    const items = menuItemsFor({
      field: { length: 4, hasSelection: false },
      documentSelection: "something else entirely",
    })
    expect(commands(items)).toEqual(["cut", "copy", "paste", "selectAll"])
  })
})

describe("every item is labelled and carries its shortcut", () => {
  it("names a shortcut for each command", () => {
    const items = menuItemsFor({
      field: { length: 3, hasSelection: true },
      documentSelection: "",
    })
    for (const item of items ?? []) {
      expect(item.label.length).toBeGreaterThan(0)
      expect(item.shortcut).toMatch(/^Ctrl\+[A-Z]$/)
    }
  })
})
