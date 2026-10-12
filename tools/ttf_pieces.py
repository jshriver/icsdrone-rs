#!/usr/bin/env python3
"""Turn Armando Marroquin's TrueType chess diagram fonts into SVG piece
sets for the GUI, plus a hatched "diagram" board.

The fonts draw a white piece as a hollow outline, so on a colored
square the square would show through its body. Each piece is written
as two layers: its silhouette (the outermost contours) filled white,
then the glyph itself in black - so white pieces are solid, and the
details inside black ones show white.

Usage (needs fontTools: pip install fonttools):

    tools/ttf_pieces.py ~/t/chess-image-2/fonts

Writes assets/pieces/<set>/{w,b}{K,Q,R,B,N,P}.svg and
assets/boards/diagram.svg. Rerun it after changing a font or the
table below; the output is committed, so building the app doesn't
need Python.
"""

import sys
import time
from pathlib import Path

from fontTools.pens.recordingPen import DecomposingRecordingPen
from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.ttLib import TTFont

# (output directory, display name, font file under the fonts dir)
SETS = [
    ("adventurer", "Adventurer", "adventur/avenfont.ttf"),
    ("alfonso-x", "Alfonso-X", "alfonso/alfoches.ttf"),
    ("condal", "Condal", "condal/condfont.ttf"),
    ("harlequin", "Harlequin", "harlequi/harlfont.ttf"),
    ("kingdom", "Kingdom", "kingdom/kingfont.ttf"),
    ("leipzig", "Leipzig", "leipzig/leipfont.ttf"),
    ("line", "Line", "line/linefont.ttf"),
    ("lucena", "Lucena", "lucena/lucefont.ttf"),
    ("magnetic", "Magnetic", "magnetic/magnfont.ttf"),
    ("mark", "Mark", "mark/markfont.ttf"),
    ("marroquin", "Marroquin", "marroqui/marrfont.ttf"),
    ("mediaeval", "Mediaeval", "medie/mvalfont.ttf"),
    ("merida-font", "Merida (font)", "merid/merifont.ttf"),
    ("millennia", "Millennia", "millennia/millfont.ttf"),
    ("motif", "Motif", "motif/motifont.ttf"),
]

# The hatched dark square of this font becomes the diagram board.
BOARD_FONT = "marroqui/marrfont.ttf"

# Piece on a light square, by our file name (see the font readmes).
GLYPHS = {
    "wK": "k", "wQ": "q", "wR": "r", "wB": "b", "wN": "n", "wP": "p",
    "bK": "l", "bQ": "w", "bR": "t", "bB": "v", "bN": "m", "bP": "o",
}
DARK_SQUARE = "+"

INK = "#000"
PAPER = "#fff"

ASSETS = Path(__file__).resolve().parent.parent / "assets"


def glyph_name(font, char):
    """The glyph for `char` in the font's Mac Roman cmap - the one these
    symbol fonts fill in for every platform."""
    cmap = font["cmap"].getcmap(1, 0) or font.getBestCmap()
    return cmap.cmap[ord(char)]


def contours(font, char):
    """The glyph's outline as a list of closed contours, each a list of
    pen operations."""
    pen = DecomposingRecordingPen(font.getGlyphSet())
    font.getGlyphSet()[glyph_name(font, char)].draw(pen)
    out, current = [], []
    for op, args in pen.value:
        current.append((op, args))
        if op in ("closePath", "endPath"):
            out.append(current)
            current = []
    return out


def polygon(contour):
    """Points along a contour, close enough for inside/outside tests."""
    points = []
    for op, args in contour:
        points.extend(args)
    return points


def inside(point, poly):
    """Even-odd ray cast: is `point` inside polygon `poly`?"""
    x, y = point
    hit = False
    for (x1, y1), (x2, y2) in zip(poly, poly[1:] + poly[:1]):
        if (y1 > y) != (y2 > y) and x < x1 + (y - y1) * (x2 - x1) / (y2 - y1):
            hit = not hit
    return hit


def outermost(cs):
    """Contours not inside any other: together, the piece's silhouette."""
    polys = [polygon(c) for c in cs]
    keep = []
    for i, poly in enumerate(polys):
        if not poly:
            continue
        if not any(j != i and other and inside(poly[0], other) for j, other in enumerate(polys)):
            keep.append(cs[i])
    return keep


def path_data(cs):
    pen = SVGPathPen(None)
    for contour in cs:
        for op, args in contour:
            getattr(pen, op)(*args)
    return pen.getCommands()


def square_box(font):
    """The dark-square glyph's bounds: the cell each piece is drawn in."""
    glyph = font["glyf"][glyph_name(font, DARK_SQUARE)]
    return glyph.xMin, glyph.yMin, glyph.xMax, glyph.yMax


def svg(box, body):
    x0, y0, x1, y1 = box
    w, h = x1 - x0, y1 - y0
    # Font units have y up; flip into SVG's y-down.
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}" width="{w}" height="{h}">'
        f'<g transform="matrix(1 0 0 -1 {-x0} {y1})">{body}</g></svg>\n'
    )


def piece_svg(font, char, box):
    cs = contours(font, char)
    return svg(
        box,
        f'<path fill="{PAPER}" d="{path_data(outermost(cs))}"/>'
        f'<path fill="{INK}" d="{path_data(cs)}"/>',
    )


def board_svg(font):
    x0, y0, x1, y1 = square_box(font)
    size = x1 - x0
    hatch = path_data(contours(font, DARK_SQUARE))
    squares = []
    for row in range(8):
        for col in range(8):
            if (row + col) % 2 == 1:
                # Row 0 is rank 8, at the top; a8 is light.
                squares.append(f'<use href="#d" transform="translate({col * size} {row * size})"/>')
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {8 * size} {8 * size}"'
        f' width="{8 * size}" height="{8 * size}">'
        f'<rect width="{8 * size}" height="{8 * size}" fill="{PAPER}"/>'
        f'<defs><path id="d" fill="{INK}" transform="matrix(1 0 0 -1 {-x0} {y1})" d="{hatch}"/></defs>'
        + "".join(squares)
        + "</svg>\n"
    )


def bar(done, total, start, width=28):
    filled = int(width * done / total)
    elapsed = time.monotonic() - start
    eta = elapsed / done * (total - done) if done else 0
    return f"[{'█' * filled}{'░' * (width - filled)}] {done}/{total}  ⏱ ETA {eta:4.1f}s"


def main():
    if len(sys.argv) != 2:
        sys.exit(f"usage: {sys.argv[0]} <chess-image-2 fonts dir>")
    fonts_dir = Path(sys.argv[1]).expanduser()

    print("♟️  Chess fonts → SVG piece sets")
    print(f"📂 fonts from {fonts_dir}")
    print(f"📦 writing to {ASSETS}\n")

    start = time.monotonic()
    biggest = (0, "")
    for i, (slug, name, file) in enumerate(SETS, 1):
        font = TTFont(fonts_dir / file)
        box = square_box(font)
        out = ASSETS / "pieces" / slug
        out.mkdir(parents=True, exist_ok=True)
        size = 0
        for piece, char in GLYPHS.items():
            data = piece_svg(font, char, box)
            (out / f"{piece}.svg").write_text(data)
            size += len(data)
        biggest = max(biggest, (size, name))
        print(f"\r  {bar(i, len(SETS), start)}  ✅ {name:<14} {size / 1024:5.1f} KiB", end="\n")

    print("\n🏁 Diagram board")
    font = TTFont(fonts_dir / BOARD_FONT)
    board = board_svg(font)
    (ASSETS / "boards").mkdir(parents=True, exist_ok=True)
    (ASSETS / "boards" / "diagram.svg").write_text(board)
    print(f"  ✅ diagram.svg  {len(board) / 1024:.1f} KiB (hatching from {BOARD_FONT})")

    print(f"\n📊 {len(SETS)} sets, {len(SETS) * len(GLYPHS)} pieces; heaviest: {biggest[1]} ({biggest[0] / 1024:.1f} KiB)")
    print(f"🎉 All done in {time.monotonic() - start:.1f}s - enjoy the new pieces!")


if __name__ == "__main__":
    main()
