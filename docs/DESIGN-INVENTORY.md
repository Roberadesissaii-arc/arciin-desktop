# Arciin Desktop — design inventory

Extracted from the reference web app before any desktop UI was written.
Source of truth: `arciin-main/apps/web`.

## Sources inspected

| Concern | Reference file |
| --- | --- |
| Theme tokens | `apps/web/app/globals.css` |
| Fonts | `apps/web/app/layout.tsx` |
| Auth design kit | `apps/web/components/auth/auth-light.tsx` |
| Onboarding shell | `apps/web/components/auth/login-page-shell.tsx` |
| Brand marks | `apps/web/components/ui/arciin-icon.tsx` |
| Pairing copy | `apps/web/components/settings/devices-panel.tsx` |

The desktop onboarding is a **light auth surface**, matching `/login` and
`/setup` — not the dark dashboard chrome.

## Typography

| Role | Value |
| --- | --- |
| Heading | Space Grotesk (`--font-space-grotesk`), local variable TTF, OFL |
| Body / UI | Geist Sans |
| Mono (codes, URLs) | Geist Mono |
| Page title | `font-heading` 28px bold, tracking-tight, `#111111` |
| Card title | `font-heading` 22px bold, tracking-tight, `#111111` |
| Subtitle | 13px, leading-relaxed, `#a0a0a0` |
| Field label | 11px, semibold, uppercase, tracking-widest, `#a0a0a0` |
| Footer / legal | 11px, `#a0a0a0` |

## Color

| Token | Value |
| --- | --- |
| Brand accent | `#ff4f12` |
| Hero gradient | `linear-gradient(155deg, #ff6a30 0%, #c82d00 100%)` |
| Primary button gradient | `linear-gradient(135deg, #ff6a30 0%, #cc2e00 100%)` |
| Page canvas | `#ffffff` (quiet surfaces `#f7f7f7`) |
| Primary text | `#222222`, headings `#111111` |
| Muted text | `#a0a0a0`, secondary `#717171` |
| Card border | `#efefef` |
| Field border / bg | `#e8e8e8` / `#f7f7f7` |
| Secondary button border | `#e5e5e5`, text `#444444` |
| Error | border `#fecaca`, bg `#fef2f2`, text `#b91c1c` |
| Info | border `#fde5b8`, bg `#fffaf0`, text `#92600a` |
| Success | border `#bbf7d0`, bg `#f0fdf4`, text `#15803d` |
| Dark shell (splash) | `#09090b` |

## Shape and spacing

| Element | Value |
| --- | --- |
| Base radius | `0.625rem` (10px) |
| Card | `rounded-3xl` (24px), border `#efefef`, shadow `0 1px 2px rgba(0,0,0,0.03)` |
| Hero panel | `rounded-[22px]` inset by 1rem on the right half |
| Field | `rounded-2xl` (16px), padding `16px / 12px` |
| Button | `rounded-2xl`, height `48px` (h-12) |
| Primary button shadow | `0 4px 18px rgba(255,79,18,0.3)` |
| Form rhythm | `space-y-4`, label→field gap `6px` |

## Behavior

| Concern | Convention |
| --- | --- |
| Icons | `lucide-react`, `size-4` inline / `size-3.5` in helper text |
| Focus | `focus-within:border-[#ff4f12]/60` on fields |
| Loading | `Loader2` + `animate-spin`, accent-tinted |
| Disabled | `disabled:opacity-60` |
| Hover (primary) | `hover:opacity-95` |
| Pairing code display | `font-mono text-4xl tracking-[0.35em]`, grouped `482 731` |
| Brand lockup | `Arciin` + accent-colored `.` in `font-heading` bold |

## Brand assets reused

Copied verbatim into `src/assets/brand/` and `src-tauri/icons/`:

- `apps/web/public/arciin-icon.svg` — black tile + orange arch
- `apps/web/public/arciin-mark.svg` — orange arch only
- `apps/web/public/fonts/space-grotesk/SpaceGrotesk-VariableFont_wght.ttf` (+ `OFL.txt`)

## Copy alignment

The server calls the flow **Settings → Devices → "Connect a device"**, and
labels the value a **pairing code**. Desktop instructions use those exact words.

## Deliberate divergences

Everything above is mirrored from the reference. These three points differ, on
purpose:

### The hero illustration

The web app's sign-in hero (`login-hero-showcase.tsx`) renders a miniature of
the dashboard, because signing in is what lands you there. This screen is the
step *before* sign-in, so a dashboard preview would promise the wrong thing.

The desktop hero instead draws what this screen is actually about — one
computer becoming trusted by one server — using the reference hero's exact
idiom: translucent white surfaces over the orange gradient
(`rgba(255,255,255,0.07)` fills, `0.12` hairlines), 9.5-11.5px type, and no
colour of its own.

### No app tile on server cards

The reference uses `ArciinIcon` as a tile in list rows. On a server card inside
an already Arciin-branded window it added nothing and cost the left column its
balance, so the card is text-led instead: name, status, address.

### Masked addresses

Server cards show `203.0.113.xxx:3002`, not the full address. The subnet and
port are what identify the network and the service; the final octet identifies
one machine and is not needed to tell saved servers apart, because the instance
name already does that. Hostnames are never masked — there is no per-machine
address in them to hide, and partially masking one would only make it
unrecognisable. See `maskAddress` in `src/lib/format.ts`.
