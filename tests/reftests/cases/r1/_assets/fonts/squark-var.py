# SPDX-License-Identifier: MIT
"""Writes SquarkVar.ttf, a variable test font with one `wght` axis.

Run with fontTools 4 installed: `python3 squark-var.py`. The capitals
E, H, I, L and T are bars whose stems thicken from weight 100 to 900, and
their advances widen with them, so a reftest sees which axis value a
renderer applied.
"""

from pathlib import Path

from fontTools.designspaceLib import AxisDescriptor, DesignSpaceDocument, SourceDescriptor
from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen
from fontTools.varLib import build

HEIGHT = 700
BAR = 80


def rect(pen, x0, y0, x1, y1):
    pen.moveTo((x0, y0))
    pen.lineTo((x0, y1))
    pen.lineTo((x1, y1))
    pen.lineTo((x1, y0))
    pen.closePath()


def glyphs(stem):
    """Each glyph as (advance, rectangles). Every master draws the same contours."""
    side = 60
    width = 360 + stem
    return {
        ".notdef": (500, [(50, 0, 450, HEIGHT)]),
        "space": (260, []),
        "I": (stem + 2 * side, [(side, 0, side + stem, HEIGHT)]),
        "H": (width + stem, [
            (side, 0, side + stem, HEIGHT),
            (width - side, 0, width + stem - side, HEIGHT),
            (side + stem, 310, width - side, 310 + BAR),
        ]),
        "L": (width - 40, [
            (side, 0, side + stem, HEIGHT),
            (side + stem, 0, width - 40 - side, BAR),
        ]),
        "E": (width - 40, [
            (side, 0, side + stem, HEIGHT),
            (side + stem, 0, width - 40 - side, BAR),
            (side + stem, 310, width - 80 - side, 310 + BAR),
            (side + stem, HEIGHT - BAR, width - 40 - side, HEIGHT),
        ]),
        "T": (width, [
            (side, HEIGHT - BAR, width - side, HEIGHT),
            ((width - stem) // 2, 0, (width + stem) // 2, HEIGHT - BAR),
        ]),
    }


def master(stem, style):
    shapes = glyphs(stem)
    order = list(shapes)
    builder = FontBuilder(1000, isTTF=True)
    builder.setupGlyphOrder(order)
    builder.setupCharacterMap({ord(" "): "space", **{ord(name): name for name in "EHILT"}})
    drawn = {}
    for name, (_, rects) in shapes.items():
        pen = TTGlyphPen(None)
        for box in rects:
            rect(pen, *box)
        drawn[name] = pen.glyph()
    builder.setupGlyf(drawn)
    metrics = {}
    glyf = builder.font["glyf"]
    for name, (advance, _) in shapes.items():
        metrics[name] = (advance, glyf[name].xMin if glyf[name].numberOfContours else 0)
    builder.setupHorizontalMetrics(metrics)
    builder.setupHorizontalHeader(ascent=800, descent=-200)
    builder.setupNameTable({"familyName": "SquarkVar", "styleName": style})
    builder.setupOS2(
        sTypoAscender=800, sTypoDescender=-200, sTypoLineGap=0,
        usWinAscent=800, usWinDescent=200, sxHeight=500, sCapHeight=HEIGHT,
    )
    builder.setupPost()
    return builder.font


def main():
    document = DesignSpaceDocument()
    axis = AxisDescriptor()
    axis.tag, axis.name = "wght", "Weight"
    axis.minimum, axis.default, axis.maximum = 100, 100, 900
    document.addAxis(axis)
    for weight, stem, style in [(100, 40, "Thin"), (900, 240, "Black")]:
        source = SourceDescriptor()
        source.font = master(stem, style)
        source.location = {"Weight": weight}
        source.styleName = style
        document.addSource(source)
    font, _, _ = build(document)
    font.save(Path(__file__).with_name("SquarkVar.ttf"))


if __name__ == "__main__":
    main()
