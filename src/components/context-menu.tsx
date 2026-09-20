/**
 * The right-click menu for this application's own screens.
 *
 * WebView2 ships a *browser* context menu, and without this it is what a
 * right-click in Arciin Desktop produces: Back, Refresh, Save as, Print, More
 * tools. In a product window those are wrong in every direction — "Save as"
 * offers to write the Backup Center out as a web page, "Refresh" reloads it,
 * and "More tools" leads to developer tools. Worse, the one thing a person
 * actually right-clicks a text field for — paste — is not on it at all, which
 * is why it reads as "right-click does nothing".
 *
 * There is no supported way to switch that menu off. Tauri 2.11 does not
 * expose wry's `with_default_context_menus`, so the only lever is the DOM:
 * `preventDefault` on the event. That works for the pages this application
 * serves itself, which is what this covers. It cannot cover the server's own
 * page inside the Arciin window — this client never injects script into a
 * remote origin, and a context menu is not a reason to start.
 *
 * What it offers is deliberately short. A context menu on a desktop app is
 * expected to do the clipboard and nothing else; anything further would be
 * inventing commands that have no home in the rest of the interface.
 */

import { useCallback, useEffect, useRef, useState } from "react"

/** How far the menu keeps from the window edge when it would overflow. */
const EDGE_GAP = 8

export type Command = "cut" | "copy" | "paste" | "selectAll"

export type Item = {
  command: Command
  label: string
  /** Shown right-aligned, because people look for the shortcut here. */
  shortcut: string
  enabled: boolean
}

/**
 * What the menu offers for a given right-click, or `null` for no menu at all.
 *
 * Split out from the component because it is the part with rules in it, and
 * the rules are worth asserting: an empty box must not offer "Select all", a
 * field with nothing selected must not offer "Cut", and a right-click on
 * ordinary chrome must produce no menu rather than an empty one.
 *
 * Paste is always offered on a field. Whether the clipboard actually has
 * anything is not knowable without reading it, and reading the clipboard to
 * decide whether to *draw* a menu would be a surprising thing for an app to
 * do on every right-click.
 */
export function menuItemsFor(target: {
  field: { length: number; hasSelection: boolean } | null
  documentSelection: string
}): Item[] | null {
  if (target.field) {
    const { length, hasSelection } = target.field
    return [
      { command: "cut", label: "Cut", shortcut: "Ctrl+X", enabled: hasSelection },
      { command: "copy", label: "Copy", shortcut: "Ctrl+C", enabled: hasSelection },
      { command: "paste", label: "Paste", shortcut: "Ctrl+V", enabled: true },
      {
        command: "selectAll",
        label: "Select all",
        shortcut: "Ctrl+A",
        enabled: length > 0,
      },
    ]
  }

  // Outside a field the only honest offer is copying what is selected. The
  // shell sets `user-select: none`, so this is usually nothing, and then no
  // menu appears at all.
  if (target.documentSelection.trim().length > 0) {
    return [{ command: "copy", label: "Copy", shortcut: "Ctrl+C", enabled: true }]
  }

  return null
}

type Position = { x: number; y: number }

type MenuState = {
  at: Position
  items: Item[]
  /** The field the menu was opened on, if it was opened on one. */
  field: HTMLInputElement | HTMLTextAreaElement | null
}

/** A text box whose selection and value we can act on. */
function editableField(
  target: EventTarget | null,
): HTMLInputElement | HTMLTextAreaElement | null {
  if (!(target instanceof HTMLElement)) return null
  const field = target.closest("input, textarea")
  if (!(field instanceof HTMLInputElement) && !(field instanceof HTMLTextAreaElement)) {
    return null
  }
  if (field.disabled || field.readOnly) return null
  // `type` covers the text-like inputs. A checkbox has no text to cut.
  if (field instanceof HTMLInputElement) {
    const textual = ["text", "search", "url", "email", "tel", "password", "number", ""]
    if (!textual.includes(field.type)) return null
  }
  return field
}

function hasSelectionIn(field: HTMLInputElement | HTMLTextAreaElement): boolean {
  return (field.selectionEnd ?? 0) > (field.selectionStart ?? 0)
}

/** Text the user has selected outside a field, if any. */
function documentSelection(): string {
  return window.getSelection()?.toString() ?? ""
}

export function ContextMenu() {
  const [menu, setMenu] = useState<MenuState | null>(null)
  const ref = useRef<HTMLDivElement | null>(null)

  const close = useCallback(() => setMenu(null), [])

  useEffect(() => {
    function onContextMenu(event: MouseEvent) {
      // Suppressed unconditionally, including where we then show nothing.
      // Leaving the browser menu on "just for the empty case" would put Save
      // as and developer tools one right-click away on most of the window.
      event.preventDefault()

      const field = editableField(event.target)
      const items = menuItemsFor({
        field: field
          ? { length: field.value.length, hasSelection: hasSelectionIn(field) }
          : null,
        documentSelection: documentSelection(),
      })

      setMenu(
        items ? { at: { x: event.clientX, y: event.clientY }, items, field } : null,
      )
    }

    document.addEventListener("contextmenu", onContextMenu)
    return () => document.removeEventListener("contextmenu", onContextMenu)
  }, [])

  // Anything that moves the page out from under the menu closes it.
  useEffect(() => {
    if (!menu) return
    function onKey(event: KeyboardEvent) {
      if (event.key === "Escape") close()
    }
    function onPointer(event: MouseEvent) {
      if (ref.current?.contains(event.target as Node)) return
      close()
    }
    document.addEventListener("keydown", onKey)
    document.addEventListener("mousedown", onPointer)
    window.addEventListener("blur", close)
    window.addEventListener("resize", close)
    document.addEventListener("scroll", close, true)
    return () => {
      document.removeEventListener("keydown", onKey)
      document.removeEventListener("mousedown", onPointer)
      window.removeEventListener("blur", close)
      window.removeEventListener("resize", close)
      document.removeEventListener("scroll", close, true)
    }
  }, [menu, close])

  // Positioned after measuring, so a menu opened near the edge turns back on
  // itself instead of being clipped by the window.
  useEffect(() => {
    const node = ref.current
    if (!node || !menu) return
    const box = node.getBoundingClientRect()
    let { x, y } = menu.at
    if (x + box.width > window.innerWidth - EDGE_GAP) {
      x = Math.max(EDGE_GAP, window.innerWidth - box.width - EDGE_GAP)
    }
    if (y + box.height > window.innerHeight - EDGE_GAP) {
      y = Math.max(EDGE_GAP, window.innerHeight - box.height - EDGE_GAP)
    }
    node.style.left = `${x}px`
    node.style.top = `${y}px`
  }, [menu])

  async function run(item: Item) {
    if (!item.enabled) return
    const field = menu?.field ?? null
    close()
    try {
      await perform(item.command, field)
    } catch {
      // A clipboard the OS refuses is not worth an error dialog over: the
      // keyboard shortcut is still there, and the menu has already closed.
    }
  }

  if (!menu) return null

  return (
    <div
      ref={ref}
      className="context-menu"
      role="menu"
      // Placed by the effect above; this only keeps it off-screen for the one
      // frame before it is measured.
      style={{ left: -9999, top: -9999 }}
    >
      {menu.items.map((item) => (
        <button
          key={item.command}
          type="button"
          role="menuitem"
          className="context-menu__item"
          disabled={!item.enabled}
          onClick={() => void run(item)}
        >
          <span>{item.label}</span>
          <span className="context-menu__shortcut">{item.shortcut}</span>
        </button>
      ))}
    </div>
  )
}

/**
 * Do the thing, against the field the menu was opened on.
 *
 * The field is re-focused first. Opening the menu moved focus to the button
 * that was clicked, and a selection that is not in the focused element is not
 * the one the clipboard acts on.
 */
async function perform(
  command: Command,
  field: HTMLInputElement | HTMLTextAreaElement | null,
): Promise<void> {
  if (field) field.focus()

  if (command === "selectAll") {
    field?.select()
    return
  }

  if (command === "copy" || command === "cut") {
    const text = field
      ? field.value.slice(field.selectionStart ?? 0, field.selectionEnd ?? 0)
      : documentSelection()
    if (!text) return
    await navigator.clipboard.writeText(text)
    if (command === "cut" && field) replaceSelection(field, "")
    return
  }

  // Paste. `document.execCommand("paste")` has not worked in Chromium for
  // years, so this reads the clipboard and edits the value directly.
  const text = await navigator.clipboard.readText()
  if (!text || !field) return
  replaceSelection(field, text)
}

/**
 * Put `text` where the selection is, and leave the caret after it.
 *
 * Written through the native value setter rather than `field.value` so React's
 * onChange still fires — assigning to `.value` on a controlled input updates
 * the DOM and leaves React's state stale, which is how a pasted pairing code
 * ends up visible in the box and absent from the form.
 */
function replaceSelection(
  field: HTMLInputElement | HTMLTextAreaElement,
  text: string,
): void {
  const start = field.selectionStart ?? field.value.length
  const end = field.selectionEnd ?? field.value.length
  const next = field.value.slice(0, start) + text + field.value.slice(end)

  const prototype =
    field instanceof HTMLTextAreaElement
      ? HTMLTextAreaElement.prototype
      : HTMLInputElement.prototype
  const setter = Object.getOwnPropertyDescriptor(prototype, "value")?.set
  if (setter) {
    setter.call(field, next)
  } else {
    field.value = next
  }

  const caret = start + text.length
  field.setSelectionRange(caret, caret)
  field.dispatchEvent(new Event("input", { bubbles: true }))
}
