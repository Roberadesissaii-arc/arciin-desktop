/**
 * The small set of controls the onboarding shell needs.
 *
 * Shapes, heights and colours come from the reference app's auth design kit
 * (`auth-light.tsx`), so a person moving from this window to the server's own
 * sign-in page sees the same buttons and fields.
 */

import type { ButtonHTMLAttributes, InputHTMLAttributes, ReactNode } from "react"
import { Loader2 } from "lucide-react"

type ButtonProps = ButtonHTMLAttributes<HTMLButtonElement> & {
  variant?: "primary" | "secondary"
  block?: boolean
  compact?: boolean
  /** Swap the leading content for a spinner and disable the control. */
  loading?: boolean
}

export function Button({
  variant = "primary",
  block,
  compact,
  loading,
  disabled,
  children,
  className,
  ...props
}: ButtonProps) {
  const classes = [
    "btn",
    `btn--${variant}`,
    block ? "btn--block" : "",
    compact ? "btn--compact" : "",
    className ?? "",
  ]
    .filter(Boolean)
    .join(" ")

  return (
    <button {...props} className={classes} disabled={disabled || loading}>
      {loading ? <Loader2 className="spin" size={16} aria-hidden /> : null}
      {children}
    </button>
  )
}

export function LinkButton({
  accent,
  className,
  children,
  ...props
}: ButtonHTMLAttributes<HTMLButtonElement> & { accent?: boolean }) {
  const classes = ["btn-link", accent ? "btn-link--accent" : "", className ?? ""]
    .filter(Boolean)
    .join(" ")
  return (
    <button type="button" {...props} className={classes}>
      {children}
    </button>
  )
}

type FieldProps = {
  id: string
  label: string
  icon?: ReactNode
  hint?: ReactNode
  invalid?: boolean
  inputProps?: InputHTMLAttributes<HTMLInputElement>
}

export function Field({ id, label, icon, hint, invalid, inputProps }: FieldProps) {
  return (
    <div className="field">
      <label className="field__label" htmlFor={id}>
        {label}
      </label>
      <div className={`field__control${invalid ? " field__control--invalid" : ""}`}>
        {icon ? (
          <span className="field__icon" aria-hidden>
            {icon}
          </span>
        ) : null}
        <input
          id={id}
          name={id}
          className="field__input"
          aria-invalid={invalid || undefined}
          {...inputProps}
        />
      </div>
      {hint ? <p className="field__hint">{hint}</p> : null}
    </div>
  )
}

export function Message({
  tone = "error",
  children,
}: {
  tone?: "error" | "info" | "success"
  children: ReactNode
}) {
  return (
    <p className={`message message--${tone}`} role={tone === "error" ? "alert" : "status"}>
      {children}
    </p>
  )
}

/** A labelled spinner used by every waiting state. */
export function Status({ children }: { children: ReactNode }) {
  return (
    <p className="status" role="status">
      <Loader2 className="spin status__spinner" size={16} aria-hidden />
      {children}
    </p>
  )
}
