"""Turn the letters "oodle" into SVG outlines, so the wordmark never needs the font installed.

Usage: python3 outline.py SpaceGrotesk_500Medium.ttf SpaceGrotesk_700Bold.ttf > oodle.json
(needs `pip install fonttools`). The fonts are SIL OFL; see crates/noodle-app/assets/fonts.
Coordinates are in a 1000-unit em with y pointing down and the baseline at y = 0.
"""
import json, sys
from fontTools.ttLib import TTFont
from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.pens.transformPen import TransformPen
from fontTools.pens.boundsPen import BoundsPen

TEXT = "oodle"
TRACKING = {"medium": -0.025, "bold": -0.03}  # em, as in the brand guidelines


def outline(path, tracking):
    font = TTFont(path)
    upm = font["head"].unitsPerEm
    gs = font.getGlyphSet()
    cmap = font.getBestCmap()
    k = 1000 / upm
    x = 0.0
    parts, ymin = [], 0.0
    for ch in TEXT:
        name = cmap[ord(ch)]
        pen = SVGPathPen(gs, ntos=lambda v: f"{v:.1f}".rstrip("0").rstrip("."))
        gs[name].draw(TransformPen(pen, (k, 0, 0, -k, x, 0)))
        parts.append(pen.getCommands())
        bp = BoundsPen(gs)
        gs[name].draw(bp)
        if bp.bounds:
            ymin = min(ymin, -bp.bounds[3] * k)
        x += gs[name].width * k + tracking * 1000
    return {"d": "".join(parts), "width": round(x, 1), "top": round(ymin, 1)}


medium, bold = sys.argv[1], sys.argv[2]
print(json.dumps({"medium": outline(medium, TRACKING["medium"]), "bold": outline(bold, TRACKING["bold"])}, indent=1))
