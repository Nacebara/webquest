"""Превью-лист готовых TGS так, как их покажет Telegram: рендер rlottie, тёмная и светлая тема,
размер в тексте (20 px) и крупно (64 px), моно перекрашен в цвет текста темы (needs_repainting).
Анимированные — несколько кадров петли в ряд.

    python3 tools/emoji/preview.py <каталог tgs> <out.png>        (pip install rlottie-python pillow)
"""

import gzip
import json
import os
import sys

from PIL import Image, ImageDraw, ImageFont
from rlottie_python import LottieAnimation

THEMES = (
    ("тёмная", (14, 22, 33), (245, 245, 245), (127, 145, 164)),
    ("светлая", (255, 255, 255), (0, 0, 0), (112, 117, 121)),
)
BIG = 64
SMALL = 20
PHASES = (0.0, 0.1, 0.2, 0.3, 0.45)
FONT = "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"


def straight(im):
    """rlottie отдаёт цвет, умноженный на прозрачность, — переводим в обычный RGBA."""
    px = im.convert("RGBA").load()
    w, h = im.size
    for y in range(h):
        for x in range(w):
            r, g, b, a = px[x, y]
            if 0 < a < 255:
                px[x, y] = (min(255, r * 255 // a), min(255, g * 255 // a), min(255, b * 255 // a), a)
    return im


def frame(doc, t, size, tint=None):
    anim = LottieAnimation.from_data(json.dumps(doc))
    total = int(doc["op"]) - int(doc["ip"])
    im = straight(anim.render_pillow_frame(frame_num=int(round(t * (total - 1))), width=size, height=size).convert("RGBA"))
    if tint:
        r, g, b = tint
        alpha = im.getchannel("A")
        im = Image.new("RGBA", im.size, (r, g, b, 255))
        im.putalpha(alpha)
    return im


def main():
    src, out = sys.argv[1], sys.argv[2]
    font = ImageFont.truetype(FONT, 13)
    head = ImageFont.truetype(FONT.replace(".ttf", "-Bold.ttf"), 18)
    groups = []
    for kind in ("color", "mono"):
        keys = sorted(f[:-4] for f in os.listdir(os.path.join(src, kind)) if f.endswith(".tgs"))
        docs = {k: json.loads(gzip.decompress(open(os.path.join(src, kind, f"{k}.tgs"), "rb").read())) for k in keys}
        groups.append((kind, docs))

    cell_w = BIG + 4 * (BIG // 2 + 6) + SMALL + 40
    cols = 6
    row_h = BIG + 26
    rows = sum((len(d) + cols - 1) // cols for _, d in groups)
    theme_h = 40 + rows * row_h + 30 * len(groups)
    sheet = Image.new("RGB", (cols * cell_w + 40, theme_h * len(THEMES) + 20), (40, 40, 44))
    draw = ImageDraw.Draw(sheet)
    y0 = 10
    for name, bg, text, muted in THEMES:
        draw.rectangle((10, y0, sheet.size[0] - 10, y0 + theme_h - 10), fill=bg)
        draw.text((24, y0 + 10), f"Эмодзи exch · тема {name}", font=head, fill=text)
        y = y0 + 40
        for kind, docs in groups:
            draw.text((24, y), "Цвет (без перекраски)" if kind == "color" else "Моно (цвет текста темы)", font=font, fill=muted)
            y += 22
            tint = text if kind == "mono" else None
            for i, (key, doc) in enumerate(docs.items()):
                cx = 24 + (i % cols) * cell_w
                cy = y + (i // cols) * row_h
                animated = int(doc["op"]) - int(doc["ip"]) > 60
                sheet.paste(frame(doc, 0.0, BIG, tint), (cx, cy), frame(doc, 0.0, BIG, tint))
                x = cx + BIG + 6
                if animated:
                    for t in PHASES[1:]:
                        im = frame(doc, t, BIG // 2, tint)
                        sheet.paste(im, (x, cy + 4), im)
                        x += BIG // 2 + 6
                else:
                    x += 4 * (BIG // 2 + 6)
                im = frame(doc, 0.0, SMALL, tint)
                sheet.paste(im, (x, cy + 8), im)
                draw.text((cx, cy + BIG + 4), key + (" · анимация" if animated else ""), font=font, fill=muted)
            y += ((len(docs) + cols - 1) // cols) * row_h + 8
        y0 += theme_h
    sheet.save(out)
    print(f"превью: {out}")


if __name__ == "__main__":
    main()
