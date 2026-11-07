"""Draws Clearway's icon (a round C with a chevron, the C alone at 16 px) into assets/icon/.

`uvx --with pillow python scripts/draw_icon.py`: 256 px PNGs, clearway.ico (green, exe), tray-<color>-<size>.bgra (raw).
"""
import math
from pathlib import Path

from PIL import Image, ImageDraw

COLORS = {"green": (46, 170, 90), "orange": (240, 150, 30), "red": (220, 60, 50), "grey": (140, 145, 150)}
TRAY_SIZES = (16, 20, 24, 32)
ICO_SIZES = (16, 20, 24, 32, 40, 48, 64, 128, 256)
OUT = Path(__file__).resolve().parent.parent / "assets" / "icon"


def icon(size, color, scale=8):
    t = size * scale
    img = Image.new("RGBA", (t, t), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    margin = t * 0.08
    thick = t * 0.16
    opening = 58
    d.arc([margin, margin, t - margin, t - margin], start=opening, end=360 - opening, fill=color, width=int(thick))
    r = thick / 2
    cx, cy, radius = t / 2, t / 2, (t - 2 * margin) / 2 - r
    for a in (opening, -opening):
        x, y = cx + radius * math.cos(math.radians(a)), cy + radius * math.sin(math.radians(a))
        d.ellipse([x - r, y - r, x + r, y + r], fill=color)
    if size > 16:
        cv = t * 0.13
        x0 = cx + radius * 0.05
        w = int(thick * 0.75)
        points = [(x0, cy - cv), (x0 + cv, cy), (x0, cy + cv)]
        d.line(points, fill=color, width=w, joint="curve")
        for px, py in points:
            d.ellipse([px - w / 2, py - w / 2, px + w / 2, py + w / 2], fill=color)
    return img.resize((size, size), Image.LANCZOS)


def bgra(img):
    r, g, b, a = img.split()
    return Image.merge("RGBA", (b, g, r, a)).tobytes()


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    for name, color in COLORS.items():
        icon(256, color).save(OUT / f"clearway-{name}-256.png")
        for size in TRAY_SIZES:
            (OUT / f"tray-{name}-{size}.bgra").write_bytes(bgra(icon(size, color)))
    green = [icon(s, COLORS["green"]) for s in ICO_SIZES]
    green[-1].save(OUT / "clearway.ico", sizes=[(s, s) for s in ICO_SIZES], append_images=green[:-1])
    print(f"icons written to {OUT}")


if __name__ == "__main__":
    main()
