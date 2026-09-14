"""Generate the Arciin Desktop application icon.

The icon is derived, not hand-drawn, so it can be regenerated whenever the
brand mark or the tile treatment changes:

    python scripts/generate-icon.py
    npx tauri icon src-tauri/icons/icon.png

Two decisions worth recording:

*Background.* A flat black tile reads as a hole on both a light and a dark
Windows taskbar. The tile instead uses a soft vertical gradient through
Arciin's own neutral greys (the `--muted` / `--card` / `--background` family in
the web app's `globals.css`), plus a faint warm bloom behind the mark. It keeps
the dark identity while giving the icon depth at 32px.

*Mark size.* The arch is drawn at ~56% of the tile height rather than filling
it. A mark with breathing room around it stays legible when Windows scales the
icon down, and it is what makes the tile read as an app icon rather than a
cropped logo.

Geometry for the arch is taken verbatim from `arciin-mark.svg` and scaled
about its own bounding-box centre, so the shape is never redrawn by hand.
"""

from __future__ import annotations

import pathlib

from PIL import Image, ImageDraw, ImageFilter

# Final icon edge, in pixels. Tauri wants a 1024 source.
SIZE = 1024

# Drawn this many times larger, then downsampled. Cheap, very effective
# antialiasing for the rounded corners and the thick stroke joins.
SS = 4

# Arciin brand accent, from `--arciin-accent` in the web app's globals.css.
ACCENT = (255, 79, 18)

# Vertical gradient stops (position, RGB). Neutral greys from Arciin's own
# token family, lightest at the top so the tile catches light like a surface.
GRADIENT = [
    (0.00, (46, 46, 50)),
    (0.50, (26, 26, 29)),
    (1.00, (15, 15, 17)),
]

# Corner radius as a fraction of the edge. Matches the squircle proportion
# Windows and macOS both expect from a modern app tile.
RADIUS_RATIO = 0.215

# The arch, exactly as in `arciin-mark.svg`.
MARK_POINTS = [(152.0, 400.0), (256.0, 108.0), (360.0, 400.0)]
MARK_STROKE = 70.0
MARK_VIEWBOX = 512.0

# Fraction of the tile height the mark's bounding box should occupy.
MARK_HEIGHT_RATIO = 0.56


def lerp(a: float, b: float, t: float) -> float:
    return a + (b - a) * t


def gradient_color(t: float) -> tuple[int, int, int]:
    """Sample the multi-stop vertical gradient at `t` in [0, 1]."""
    for i in range(len(GRADIENT) - 1):
        p0, c0 = GRADIENT[i]
        p1, c1 = GRADIENT[i + 1]
        if t <= p1 or i == len(GRADIENT) - 2:
            span = (t - p0) / (p1 - p0) if p1 > p0 else 0.0
            span = min(max(span, 0.0), 1.0)
            return tuple(round(lerp(c0[j], c1[j], span)) for j in range(3))
    return GRADIENT[-1][1]


def rounded_mask(edge: int, radius: int) -> Image.Image:
    mask = Image.new("L", (edge, edge), 0)
    ImageDraw.Draw(mask).rounded_rectangle((0, 0, edge - 1, edge - 1), radius, fill=255)
    return mask


def scaled_mark(edge: int) -> tuple[list[tuple[float, float]], float]:
    """The arch, scaled to `MARK_HEIGHT_RATIO` and centred in an `edge` tile."""
    half = MARK_STROKE / 2
    ys = [p[1] for p in MARK_POINTS]
    xs = [p[0] for p in MARK_POINTS]
    box_top, box_bottom = min(ys) - half, max(ys) + half
    box_left, box_right = min(xs) - half, max(xs) + half

    # Scale so the stroked bounding box is the requested fraction of the tile.
    scale = (MARK_HEIGHT_RATIO * MARK_VIEWBOX) / (box_bottom - box_top)
    centre_x = (box_left + box_right) / 2
    centre_y = (box_top + box_bottom) / 2

    unit = edge / MARK_VIEWBOX
    points = [
        (
            (MARK_VIEWBOX / 2 + (x - centre_x) * scale) * unit,
            (MARK_VIEWBOX / 2 + (y - centre_y) * scale) * unit,
        )
        for x, y in MARK_POINTS
    ]
    return points, MARK_STROKE * scale * unit


def build(edge: int) -> Image.Image:
    # --- Tile background -------------------------------------------------
    tile = Image.new("RGB", (edge, edge))
    draw = ImageDraw.Draw(tile)
    for y in range(edge):
        draw.line([(0, y), (edge, y)], fill=gradient_color(y / (edge - 1)))

    # --- Warm bloom behind the mark --------------------------------------
    # Keeps the tile unmistakably Arciin without lighting it up.
    bloom = Image.new("L", (edge, edge), 0)
    span = edge * 0.26
    ImageDraw.Draw(bloom).ellipse(
        (edge / 2 - span, edge / 2 - span, edge / 2 + span, edge / 2 + span),
        # Just enough warmth to read as intentional. Stronger than this and the
        # tile looks like it is lit from inside rather than simply dark.
        fill=24,
    )
    bloom = bloom.filter(ImageFilter.GaussianBlur(edge * 0.11))
    tile = Image.composite(Image.new("RGB", (edge, edge), ACCENT), tile, bloom)

    # --- The arch --------------------------------------------------------
    points, stroke = scaled_mark(edge)
    mark = Image.new("RGBA", (edge, edge), (0, 0, 0, 0))
    mark_draw = ImageDraw.Draw(mark)
    # `joint="curve"` rounds the apex; PIL squares off line ends, so the caps
    # are drawn as circles to match the SVG's `stroke-linecap="round"`.
    mark_draw.line(points, fill=ACCENT + (255,), width=round(stroke), joint="curve")
    for x, y in (points[0], points[-1]):
        r = stroke / 2
        mark_draw.ellipse((x - r, y - r, x + r, y + r), fill=ACCENT + (255,))

    tile = tile.convert("RGBA")
    tile.alpha_composite(mark)

    # --- Top edge highlight ----------------------------------------------
    # A one-pixel lighter rim along the top, the way a real surface catches
    # light. Invisible as such, but the tile looks flat without it.
    radius = round(edge * RADIUS_RATIO)
    rim = Image.new("RGBA", (edge, edge), (0, 0, 0, 0))
    ImageDraw.Draw(rim).rounded_rectangle(
        (0, 0, edge - 1, edge - 1),
        radius,
        outline=(255, 255, 255, 34),
        width=max(1, round(edge * 0.004)),
    )
    fade = Image.linear_gradient("L").resize((edge, edge)).point(lambda v: 255 - v)
    rim.putalpha(Image.composite(rim.getchannel("A"), Image.new("L", (edge, edge), 0), fade))
    tile.alpha_composite(rim)

    tile.putalpha(rounded_mask(edge, radius))
    return tile


def main() -> None:
    icon = build(SIZE * SS).resize((SIZE, SIZE), Image.LANCZOS)
    out = pathlib.Path(__file__).resolve().parent.parent / "src-tauri" / "icons" / "icon.png"
    out.parent.mkdir(parents=True, exist_ok=True)
    icon.save(out)
    print(f"wrote {out} ({icon.size[0]}x{icon.size[1]})")


if __name__ == "__main__":
    main()
