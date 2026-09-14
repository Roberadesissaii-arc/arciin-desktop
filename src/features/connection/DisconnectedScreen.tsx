/**
 * Where the user lands after this computer is disconnected.
 *
 * Most often they did it themselves, from Settings → Devices inside the Arciin
 * window. The alternative to this screen is leaving them looking at a web app
 * quietly failing every request, which reads as a broken product rather than
 * the thing they just asked for.
 *
 * The copy is deliberate about what did *not* happen: nothing on this PC was
 * deleted. That is the first question anyone asks after disconnecting a backup
 * client, and answering it before they ask is the whole job of this screen.
 */

import { PlugZap, ServerCog, ShieldCheck } from "lucide-react"

import { Button, LinkButton } from "@/components/ui"
import { useOnboarding } from "@/stores/onboarding"

export function DisconnectedScreen() {
  const target = useOnboarding((state) => state.disconnectedServer)
  const goManual = useOnboarding((state) => state.goManual)
  const search = useOnboarding((state) => state.search)

  return (
    <div className="stack">
      <div className="disconnect__badge" aria-hidden>
        <PlugZap size={20} />
      </div>

      <div>
        <h1 className="title">This computer is disconnected</h1>
        <p className="subtitle">
          {target
            ? `${target} no longer recognises this computer.`
            : "Your Arciin server no longer recognises this computer."}{" "}
          Pair again to reconnect it.
        </p>
      </div>

      <div className="card stack stack--tight">
        <p className="disconnect__point">
          <ShieldCheck size={14} aria-hidden />
          <span>
            <strong>Nothing on this PC was deleted.</strong> Your Desktop,
            Documents, Pictures, Videos and Music are exactly as they were.
          </span>
        </p>
        <p className="disconnect__point">
          <ServerCog size={14} aria-hidden />
          <span>
            Files already backed up are still on your Arciin server. This
            computer just can&rsquo;t reach them until it&rsquo;s paired again.
          </span>
        </p>
      </div>

      <div className="stack stack--tight">
        <Button block onClick={goManual}>
          Pair Again
        </Button>
        <div className="centered">
          <LinkButton onClick={() => void search()}>Choose another server</LinkButton>
        </div>
      </div>
    </div>
  )
}
