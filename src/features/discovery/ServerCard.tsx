/**
 * One server on screen.
 *
 * What is shown is deliberately limited to what the discovery manifest is
 * allowed to carry: a name, where it lives, and whether it can be paired.
 * Nothing about the database, storage paths, licence, users or internal ids
 * reaches this component, because none of it reaches the client at all.
 */

import { ChevronRight, HardDrive, Wifi } from "lucide-react"

import { Button } from "@/components/ui"
import { hostLabel, maskAddress } from "@/lib/format"
import type { SavedServer, VerifiedServer } from "@/types"

export function FoundServerCard({
  server,
  onConnect,
}: {
  server: VerifiedServer
  onConnect: () => void
}) {
  return (
    <div className="server fade-up">
      <div className="server__text">
        <p className="server__name">{server.name}</p>
        <p className="server__meta">
          <Wifi size={12} aria-hidden />
          Local Network
        </p>
        <p className="server__address">{maskAddress(server.displayAddress)}</p>
      </div>
      <div className="server__actions">
        <Button compact onClick={onConnect}>
          Connect
          <ChevronRight size={15} aria-hidden />
        </Button>
      </div>
    </div>
  )
}

/** A server this computer has already paired with. */
export function SavedServerCard({
  server,
  onConnect,
  onForget,
  busy,
}: {
  server: SavedServer
  onConnect: () => void
  onForget: () => void
  busy: boolean
}) {
  return (
    <div className="server fade-up">
      <div className="server__text">
        <p className="server__name">{server.name}</p>
        <p className="server__meta">
          <HardDrive size={12} aria-hidden />
          {server.revoked ? "Needs pairing again" : "Saved Server"}
        </p>
        <p className="server__address">{maskAddress(hostLabel(server.baseUrl))}</p>
      </div>
      <div className="server__actions">
        {/*
         * Forget comes first so the primary action stays flush right, in the
         * same column as Connect on a found server. Scanning a stack of cards,
         * the action you want is then always in one predictable place.
         */}
        <Button compact variant="secondary" onClick={onForget} disabled={busy}>
          Forget
        </Button>
        <Button compact onClick={onConnect} disabled={busy}>
          {server.revoked ? "Pair Again" : "Connect"}
          {server.revoked ? null : <ChevronRight size={15} aria-hidden />}
        </Button>
      </div>
    </div>
  )
}
