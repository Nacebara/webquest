#!/usr/bin/env python3
"""Сверка векторных эмодзи с эталоном.

Каждый .tgs рисуется rlottie — тем же движком, что у Telegram, — и сравнивается с
кадрами SVG (frames.js) попиксельно. Заодно проверяются требования Telegram к TGS:
холст 512×512, 60 кадров/с, не дольше 3 с, файл до 64 КБ, только разрешённые слои.

    pip install rlottie-python pillow
    node tools/emoji/frames.js <эталоны> 256
    python3 tools/emoji/verify_tgs.py assets/emoji <эталоны> [лист.png]
"""

import gzip
import json
import os
import sys

import numpy as np
from PIL import Image, ImageChops, ImageDraw, ImageFilter
from rlottie_python import LottieAnimation

LIMIT = 64 * 1024
# Средняя разница на пиксель (0..255) и доля заметно разных пикселей — после лёгкого
# размытия: сглаживание краёв у rlottie и librsvg разное, а сдвиг формы или пропавшая
# деталь размытие переживают.
# Разница до QUANT уровней из 255 не считается: столько даёт 8-битный градиент и сведение слоёв
# в rlottie — ровный сдвиг цвета по всей плитке, глазом не виден. Форма и сдвиги контура дают на
# краях десятки уровней и в среднее попадают почти целиком. Порог MAX_MEAN подобран так, что
# сдвиг контура на 1 px из 256 уже не проходит (≈ 1,0–1,5), а чистые кадры дают до ≈ 0,35.
QUANT = 3
MAX_MEAN = 0.8
MAX_BAD = 0.004
FORBIDDEN_LAYER_TYPES = {1: "solid", 2: "image", 5: "text"}


def check_doc(name, doc, raw_len):
    errs = []
    if doc.get("tgs") != 1:
        errs.append("нет tgs: 1")
    if (doc.get("w"), doc.get("h")) != (512, 512):
        errs.append(f"холст {doc.get('w')}×{doc.get('h')}")
    if doc.get("fr") != 60:
        errs.append(f"fps {doc.get('fr')}")
    if doc.get("op", 0) - doc.get("ip", 0) > 180:
        errs.append("дольше 3 с")
    if raw_len > LIMIT:
        errs.append(f"{raw_len} байт > 64 КБ")
    for layer in doc["layers"]:
        if layer.get("ty") in FORBIDDEN_LAYER_TYPES:
            errs.append(f"слой {FORBIDDEN_LAYER_TYPES[layer['ty']]}")
        if layer.get("masksProperties") or layer.get("hasMask"):
            errs.append("маска")
        if layer.get("ef"):
            errs.append("эффекты")
        if layer.get("ddd"):
            errs.append("3D")

        def walk(items):
            for it in items:
                if it.get("ty") in ("mm", "rp", "gs", "sr"):
                    errs.append(f"запрещённая фигура {it['ty']}")
                if it.get("ty") == "gr":
                    walk(it["it"])
                for v in it.values():
                    if isinstance(v, dict) and "x" in v and isinstance(v.get("x"), str):
                        errs.append("выражение")

        walk(layer.get("shapes", []))
    return [f"{name}: {e}" for e in errs]


def straight(im):
    """rlottie отдаёт цвет, умноженный на прозрачность, — возвращаем обычный RGBA."""
    a = np.asarray(im.convert("RGBA")).astype(np.float32)
    alpha = a[..., 3:4]
    rgb = np.where(alpha > 0, a[..., :3] * 255.0 / np.maximum(alpha, 1), 0)
    return Image.fromarray(np.concatenate([np.clip(rgb, 0, 255), alpha], axis=2).astype(np.uint8), "RGBA")


# Фоны чатов Telegram: тёмная тема и светлая. Значок должен совпасть с эталоном на обоих.
BACKGROUNDS = ((14, 22, 33, 255), (255, 255, 255, 255))


def on_bg(im, color):
    bg = Image.new("RGBA", im.size, color)
    return Image.alpha_composite(bg, im.convert("RGBA")).convert("RGB")


def diff(a, b):
    """Разница кадров на тёмном и светлом фоне Telegram — худшая из двух."""
    blur = ImageFilter.GaussianBlur(1.2)
    worst = None
    total = a.size[0] * a.size[1]
    for color in BACKGROUNDS:
        d = ImageChops.difference(on_bg(a, color).filter(blur), on_bg(b, color).filter(blur)).convert("L")
        hist = d.histogram()
        mean = sum(max(0, i - QUANT) * c for i, c in enumerate(hist)) / total
        bad = sum(hist[40:]) / total
        if worst is None or (mean, bad) > worst[:2]:
            worst = (mean, bad, d)
    return worst


def main():
    tgs_dir, ref_dir = sys.argv[1], sys.argv[2]
    sheet_path = sys.argv[3] if len(sys.argv) > 3 else None
    index = json.load(open(os.path.join(ref_dir, "index.json")))
    errors = []
    worst = []
    rows = []
    for kind in ("mono", "color"):
        for item in index[kind]:
            key = item["key"]
            path = os.path.join(tgs_dir, kind, f"{key}.tgs")
            raw = open(path, "rb").read()
            doc = json.loads(gzip.decompress(raw))
            errors += check_doc(f"{kind}/{key}", doc, len(raw))
            anim = LottieAnimation.from_data(json.dumps(doc))
            for f in item["frames"]:
                ref = Image.open(os.path.join(ref_dir, kind, f"{key}_{f}.png"))
                got = straight(anim.render_pillow_frame(frame_num=f, width=ref.size[0], height=ref.size[1]))
                mean, bad, d = diff(ref, got)
                worst.append((mean, bad, f"{kind}/{key}@{f}"))
                if mean > MAX_MEAN or bad > MAX_BAD:
                    errors.append(f"{kind}/{key} кадр {f}: средняя разница {mean:.2f}, заметных пикселей {bad:.2%}")
                if sheet_path and (f == item["frames"][len(item["frames"]) // 2]):
                    rows.append((f"{kind}/{key}@{f}", ref, got, d))
    worst.sort(reverse=True)
    print("наибольшие расхождения:")
    for mean, bad, name in worst[:8]:
        print(f"  {name}: средняя {mean:.2f}, заметных {bad:.2%}")
    if sheet_path:
        cell = 128
        cols = 4
        sheet = Image.new("RGB", (cols * (3 * cell + 24), ((len(rows) + cols - 1) // cols) * (cell + 22)), (14, 22, 33))
        draw = ImageDraw.Draw(sheet)
        for n, (name, ref, got, d) in enumerate(rows):
            x = (n % cols) * (3 * cell + 24)
            y = (n // cols) * (cell + 22)
            for k, im in enumerate((on_bg(ref, BACKGROUNDS[0]), on_bg(got, BACKGROUNDS[0]), d.point(lambda v: min(255, v * 4)))):
                sheet.paste(im.convert("RGB").resize((cell, cell)), (x + k * cell, y))
            draw.text((x + 2, y + cell + 4), name, fill=(140, 160, 180))
        sheet.save(sheet_path)
    if errors:
        print("ОШИБКИ:")
        for e in errors:
            print("  " + e)
        sys.exit(1)
    print(f"ок: {sum(len(i['frames']) for k in ('mono', 'color') for i in index[k])} кадров совпадают с эталоном")


if __name__ == "__main__":
    main()
