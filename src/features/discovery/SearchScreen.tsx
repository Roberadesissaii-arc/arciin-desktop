/**
 * "Find your Arciin" - the first screen, and the results that follow it.
 *
 * mDNS is a convenience, not a requirement (the server reports its support as
 * partial). So finding nothing is presented as an ordinary outcome with an
 * obvious next step, never as a failure.
 */

import { RefreshCw } from "lucide-react"

import { FooterNote } from "@/components/footer-note"
import { ArciinMark } from "@/components/brand"
import { Button, LinkButton, Message, Status } from "@/components/ui"
import { FoundServerCard, SavedServerCard } from "@/features/discovery/ServerCard"
import { useOnboarding } from "@/stores/onboarding"

export function SearchScreen() {
  const { step, found, saved, busy, error } = useOnboarding()
  const { search, goManual, chooseServer, connect, forget, clearError } = useOnboarding()

  const searching = step === "searching"
  const nothingFound = !searching && found.length === 0 && saved.length === 0

  return (
    <div className="stack">
      <div>
        <h1 className="title">Find your Arciin</h1>
        <p className="subtitle">
          Connect this computer to your private Arciin server.
        </p>
      </div>

      {searching ? (
        <div className="stack" style={{ alignItems: "center", gap: 20, padding: "12px 0" }}>
          <div className="radar">
            <span className="radar__ring" />
            <span className="radar__ring" />
            <span className="radar__ring" />
            <ArciinMark className="radar__mark" />
          </div>
          <Status>Searching your network&hellip;</Status>
        </div>
      ) : null}

      {error ? (
        <Message tone={error.code === "DEVICE_REVOKED" ? "info" : "error"}>
          {error.message}
        </Message>
      ) : null}

      {saved.length > 0 ? (
        <section>
          <p className="section-label">Saved Servers</p>
          {saved.map((server) => (
            <SavedServerCard
              key={server.serverId}
              server={server}
              busy={busy}
              onConnect={() => {
                clearError()
                if (server.revoked) {
                  // A revoked device has to go through pairing again, which
                  // starts from a fresh look at the address.
                  goManual()
                } else {
                  void connect(server.serverId)
                }
              }}
              onForget={() => void forget(server.serverId)}
            />
          ))}
        </section>
      ) : null}

      {!searching && found.length > 0 ? (
        <section>
          <p className="section-label">Found Servers</p>
          {found.map((server) => (
            <FoundServerCard
              key={server.serverId}
              server={server}
              onConnect={() => chooseServer(server)}
            />
          ))}
        </section>
      ) : null}

      {nothingFound ? (
        <div className="card centered stack stack--tight">
          <p style={{ margin: 0, fontSize: 13.5, color: "var(--text-secondary)" }}>
            No Arciin servers found automatically.
          </p>
          <p style={{ margin: 0, fontSize: 12, color: "var(--text-muted)", lineHeight: 1.6 }}>
            Automatic discovery needs your server to advertise itself on this
            network. Entering its address works either way.
          </p>
        </div>
      ) : null}

      {!searching ? (
        <div className="stack stack--tight">
          <Button block onClick={goManual}>
            Enter Server Address
          </Button>
          <div className="row row--between">
            <LinkButton onClick={() => void search()} disabled={busy}>
              <span className="row" style={{ gap: 6 }}>
                <RefreshCw size={13} aria-hidden />
                Search again
              </span>
            </LinkButton>
            <span style={{ fontSize: 11.5, color: "var(--text-muted)" }}>
              Can&rsquo;t find your server?
            </span>
          </div>
        </div>
      ) : null}

      <FooterNote>Private by design &mdash; your account and files stay on your server.</FooterNote>
    </div>
  )
}
