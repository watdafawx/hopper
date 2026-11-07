"""Measure a screenshot instead of judging it by eye or by the code.

Typical use: you centred a number in a badge, an icon in a button, a label in
a chip. Take a screenshot (at 1x or the app's zoom), crop the CONTAINER, and
measure the gap on each side of the CONTENT:

  python -I measure_shot.py shot.png --box 120 40 148 60
  python -I measure_shot.py shot.png --box 120 40 148 60 --zoom crop.png

--box x0 y0 x1 y1   the container (pixels in the screenshot, x1/y1 exclusive).
--bg #rrggbb        the container's fill; default: the most common colour in
                    the box.
--tol N             how far a pixel may differ from the fill and still count as
                    fill (0-255 per channel, default 24; raise it for
                    anti-aliased text on a gradient).
--round             the container is a circle or a pill: ignore the box's
                    corners (pixels outside the ellipse inscribed in the box,
                    shrunk 1 px), which show what is BEHIND the container.
                    Always use it for round badges, dots and avatars; make the
                    box fit the container tightly.
--zoom out.png      also save the crop enlarged 8x (nearest neighbour) so you
                    can LOOK at it and attach it to the card.

Prints the content's bounding box and the four gaps (left, right, top,
bottom), and says CENTRED when left/right and top/bottom each differ by at most
1 px. For text, an odd gap difference of 1 px is the font's rounding: report
the numbers anyway. Pillow is needed (`pip install pillow`).

To compare two elements that must line up (say a badge on one icon and the
badge on the History button), run it once per element with a box of the same
size and compare the printed gaps: they should match within 1 px.
"""
import argparse
import sys
from collections import Counter

from PIL import Image


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("image")
    ap.add_argument("--box", nargs=4, type=int, required=True, metavar=("X0", "Y0", "X1", "Y1"))
    ap.add_argument("--bg", default=None)
    ap.add_argument("--tol", type=int, default=24)
    ap.add_argument("--round", action="store_true")
    ap.add_argument("--zoom", default=None)
    a = ap.parse_args()

    im = Image.open(a.image).convert("RGB")
    x0, y0, x1, y1 = a.box
    if not (0 <= x0 < x1 <= im.width and 0 <= y0 < y1 <= im.height):
        print(f"box {a.box} is outside the image ({im.width}x{im.height})")
        return 2
    crop = im.crop((x0, y0, x1, y1))
    w, h = crop.size
    px = crop.load()

    if a.bg:
        s = a.bg.lstrip("#")
        bg = tuple(int(s[i : i + 2], 16) for i in (0, 2, 4))
    else:
        bg = Counter(crop.getdata()).most_common(1)[0][0]

    def is_content(p):
        return any(abs(p[i] - bg[i]) > a.tol for i in range(3))

    def inside(x, y):
        if not a.round:
            return True
        rx, ry = max(w / 2 - 1, 0.5), max(h / 2 - 1, 0.5)
        return ((x + 0.5 - w / 2) / rx) ** 2 + ((y + 0.5 - h / 2) / ry) ** 2 <= 1

    xs, ys = [], []
    for y in range(h):
        for x in range(w):
            if inside(x, y) and is_content(px[x, y]):
                xs.append(x)
                ys.append(y)
    if not xs:
        print(f"nothing but the fill {bg} in the box: nothing to measure")
        return 2
    l, r, t, b = min(xs), max(xs), min(ys), max(ys)
    gl, gr, gt, gb = l, w - 1 - r, t, h - 1 - b
    print(f"box {w}x{h} px, fill rgb{bg}, tolerance {a.tol}")
    print(f"content: x {l}..{r} ({r - l + 1} px wide), y {t}..{b} ({b - t + 1} px tall)")
    print(f"gaps: left {gl}, right {gr}, top {gt}, bottom {gb}")
    dx, dy = abs(gl - gr), abs(gt - gb)
    print(f"off-centre: horizontal {dx} px, vertical {dy} px -> " + ("CENTRED" if dx <= 1 and dy <= 1 else "NOT CENTRED"))
    if a.zoom:
        crop.resize((w * 8, h * 8), Image.NEAREST).save(a.zoom)
        print(f"zoomed crop saved: {a.zoom}")
    return 0 if dx <= 1 and dy <= 1 else 1


if __name__ == "__main__":
    sys.exit(main())
