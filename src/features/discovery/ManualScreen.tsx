/**
 * Manual address entry.
 *
 * A first-class path, not a fallback: on a network where multicast does not
 * propagate - which includes most Docker bridge setups - this is the only way
 * in, and it has to feel as considered as the automatic one.
 */

import { type FormEvent, useState } from "react"
import { ArrowLeft, Globe } from "lucide-react"

import { Button, Field, LinkButton, Message } from "@/components/ui"
import { useOnboarding } from "@/stores/onboarding"

export function ManualScreen() {
  const { busy, error } = useOnboarding()
  const { submitAddress, goResults, clearError } = useOnboarding()
  const [address, setAddress] = useState("")

  function onSubmit(event: FormEvent) {
    event.preventDefault()
    const trimmed = address.trim()
    if (!trimmed || busy) return
    void submitAddress(trimmed)
  }

  return (
    <form className="stack" onSubmit={onSubmit}>
      <div>
        <h1 className="title">Enter server address</h1>
        <p className="subtitle">
          The address you use to open Arciin in a browser on this network.
        </p>
      </div>

      <Field
        id="address"
        label="Server address"
        icon={<Globe size={16} />}
        invalid={Boolean(error)}
        hint="For example 192.168.1.50, arciin.local, or https://arciin.example.com"
        inputProps={{
          value: address,
          onChange: (event) => {
            setAddress(event.target.value)
            if (error) clearError()
          },
          placeholder: "192.168.1.50",
          autoFocus: true,
          autoComplete: "off",
          spellCheck: false,
          inputMode: "url",
          disabled: busy,
        }}
      />

      {error ? <Message>{error.message}</Message> : null}

      <Button type="submit" block loading={busy} disabled={!address.trim()}>
        {busy ? "Checking…" : "Continue"}
      </Button>

      <div className="centered">
        <LinkButton onClick={goResults} disabled={busy}>
          <span className="row" style={{ gap: 6 }}>
            <ArrowLeft size={13} aria-hidden />
            Back to search
          </span>
        </LinkButton>
      </div>
    </form>
  )
}
