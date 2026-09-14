/**
 * Pairing.
 *
 * The instructions name the exact path on the server - Settings -> Devices ->
 * "Connect a device" - using the same words the server's own UI uses, so the
 * two halves of the task read as one product rather than two.
 */

import { type FormEvent, useState } from "react"
import { ArrowLeft, Laptop } from "lucide-react"

import { Button, Field, LinkButton, Message } from "@/components/ui"
import { CodeInput } from "@/features/pairing/CodeInput"
import { useOnboarding } from "@/stores/onboarding"

export function PairingScreen() {
  const { target, busy, error, deviceName } = useOnboarding()
  const { submitCode, setDeviceName, goResults, clearError } = useOnboarding()
  const [code, setCode] = useState("")
  const [editingName, setEditingName] = useState(false)

  if (!target) return null

  const complete = code.length === 6

  function onSubmit(event: FormEvent) {
    event.preventDefault()
    if (!complete || busy) return
    void submitCode(code)
  }

  return (
    <form className="stack" onSubmit={onSubmit}>
      <div>
        <h1 className="title">Connect to {target.name}</h1>
        <p className="subtitle">
          On your Arciin server open <strong>Settings &rarr; Devices</strong>,
          choose <strong>Connect a device</strong>, then enter the code it shows.
        </p>
      </div>

      <div className="stack stack--tight">
        <p className="section-label" style={{ margin: 0, textAlign: "center" }}>
          Pairing code
        </p>
        <CodeInput
          value={code}
          onChange={(next) => {
            setCode(next)
            if (error) clearError()
          }}
          onComplete={(next) => void submitCode(next)}
          invalid={Boolean(error)}
          disabled={busy}
        />
      </div>

      {error ? <Message>{error.message}</Message> : null}

      {editingName ? (
        <Field
          id="device-name"
          label="This computer will appear as"
          icon={<Laptop size={16} />}
          inputProps={{
            value: deviceName,
            onChange: (event) => setDeviceName(event.target.value),
            maxLength: 80,
            autoFocus: true,
            disabled: busy,
          }}
        />
      ) : (
        <p className="centered" style={{ margin: 0, fontSize: 12, color: "var(--text-muted)" }}>
          This computer will appear as{" "}
          <strong style={{ color: "var(--text-secondary)" }}>{deviceName}</strong>.{" "}
          <LinkButton accent onClick={() => setEditingName(true)} disabled={busy}>
            Change
          </LinkButton>
        </p>
      )}

      <Button type="submit" block loading={busy} disabled={!complete}>
        {busy ? "Pairing…" : "Pair Device"}
      </Button>

      <div className="centered">
        <LinkButton onClick={goResults} disabled={busy}>
          <span className="row" style={{ gap: 6 }}>
            <ArrowLeft size={13} aria-hidden />
            Choose a different server
          </span>
        </LinkButton>
      </div>
    </form>
  )
}
