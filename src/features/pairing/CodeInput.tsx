/**
 * The six-digit pairing code input.
 *
 * Six separate boxes, grouped `482 731` to match how the server displays the
 * code in Settings -> Devices, so what the user reads and what they type have
 * the same shape.
 *
 * Typing, pasting, Backspace and arrow keys all have to work - a code entry
 * that only accepts one of those is the kind of small failure that makes an
 * install feel unfinished.
 */

import { type ClipboardEvent, type KeyboardEvent, useEffect, useRef } from "react"

import { normalizeCode } from "@/lib/format"

const LENGTH = 6

export function CodeInput({
  value,
  onChange,
  onComplete,
  invalid,
  disabled,
}: {
  value: string
  onChange: (next: string) => void
  onComplete?: (code: string) => void
  invalid?: boolean
  disabled?: boolean
}) {
  const refs = useRef<Array<HTMLInputElement | null>>([])

  useEffect(() => {
    refs.current[0]?.focus()
  }, [])

  const digits = value.padEnd(LENGTH, " ").slice(0, LENGTH).split("")

  function commit(next: string) {
    const clean = normalizeCode(next)
    onChange(clean)
    if (clean.length === LENGTH) onComplete?.(clean)
    return clean
  }

  function focusCell(index: number) {
    const clamped = Math.max(0, Math.min(LENGTH - 1, index))
    const cell = refs.current[clamped]
    cell?.focus()
    cell?.select()
  }

  function onCellInput(index: number, raw: string) {
    const typed = raw.replace(/\D/g, "")
    if (!typed) return

    // Typing into a full box replaces that digit; typing several at once
    // (a fast paste into one cell) fills forward from here.
    const chars = value.split("")
    for (let offset = 0; offset < typed.length && index + offset < LENGTH; offset += 1) {
      chars[index + offset] = typed[offset]
    }
    const next = commit(chars.join("").replace(/\s/g, ""))
    focusCell(index + typed.length)
    if (next.length === LENGTH) refs.current[LENGTH - 1]?.blur()
  }

  function onKeyDown(index: number, event: KeyboardEvent<HTMLInputElement>) {
    if (event.key === "Backspace") {
      event.preventDefault()
      const chars = value.split("")
      if (chars[index]) {
        // Clear this box and stay put.
        chars[index] = ""
        commit(chars.join(""))
      } else {
        // Already empty: step back and clear the previous one.
        chars[index - 1] = ""
        commit(chars.join(""))
        focusCell(index - 1)
      }
      return
    }

    if (event.key === "ArrowLeft") {
      event.preventDefault()
      focusCell(index - 1)
    } else if (event.key === "ArrowRight") {
      event.preventDefault()
      focusCell(index + 1)
    } else if (event.key === "Home") {
      event.preventDefault()
      focusCell(0)
    } else if (event.key === "End") {
      event.preventDefault()
      focusCell(LENGTH - 1)
    }
  }

  function onPaste(event: ClipboardEvent<HTMLInputElement>) {
    event.preventDefault()
    // Accept whatever shape the code was copied in: "482731", "482 731",
    // "482-731" all normalize to the same six digits.
    const pasted = event.clipboardData.getData("text")
    const next = commit(pasted)
    focusCell(next.length)
  }

  return (
    <div
      className="code"
      role="group"
      aria-label="Pairing code, six digits"
      onPaste={onPaste}
    >
      {digits.map((digit, index) => (
        <div key={index} style={{ display: "contents" }}>
          {index === 3 ? <span className="code__gap" aria-hidden /> : null}
          <input
            ref={(element) => {
              refs.current[index] = element
            }}
            className={`code__cell${invalid ? " code__cell--invalid" : ""}`}
            value={digit.trim()}
            onChange={(event) => onCellInput(index, event.target.value)}
            onKeyDown={(event) => onKeyDown(index, event)}
            onFocus={(event) => event.target.select()}
            inputMode="numeric"
            autoComplete="one-time-code"
            // maxLength 1 keeps each box single-digit, but a paste still
            // arrives whole through onPaste above.
            maxLength={1}
            aria-label={`Digit ${index + 1}`}
            disabled={disabled}
          />
        </div>
      ))}
    </div>
  )
}
