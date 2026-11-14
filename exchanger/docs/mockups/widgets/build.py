"""Страница предпросмотра виджетов Bot API 10.4 + проверка лимитов Telegram.

    EXCH_JSON=1 node tools/emoji/tgs.js /tmp/emoji                 # как для template.html
    python3 docs/mockups/widgets/build.py /tmp/emoji /tmp/exch-widgets.html

Лимиты (core.telegram.org/bots/widgets, «Limits»): HTML до 32768 байт и до 4096 байт после zlib
уровня 9 (сервер считает так же), состояние chat_data до 1024 байт UTF-8. Превышение — код 1.
"""

import json
import os
import re
import sys
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))
HTML_MAX, ZIP_MAX, DATA_MAX = 32768, 4096, 1024


def live(v, now=1_763_040_000):
    """«@-120» → unix-время; для подсчёта размера берём 10-значное число, как в бою."""
    if isinstance(v, str) and re.fullmatch(r"@[+-]\d+", v):
        return now + int(v[1:])
    if isinstance(v, list):
        return [live(x, now) for x in v]
    if isinstance(v, dict):
        return {k: live(x, now) for k, x in v.items()}
    return v


def main():
    emoji_dir, out = sys.argv[1], sys.argv[2]
    with open(os.path.join(HERE, "samples.json"), encoding="utf-8") as f:
        samples = json.load(f)
    demos = {k: v for k, v in samples.items() if isinstance(v, dict) and ("w" in v or "states" in v)}
    states = {k: demos[v.get("same", k)]["states"] for k, v in demos.items()}
    src, sizes, bad = {}, {}, []
    for name in sorted({v["w"] for v in demos.values() if v.get("w")}):
        with open(os.path.join(HERE, f"{name}.html"), encoding="utf-8") as f:
            html = f.read()
        raw = html.encode("utf-8")
        packed = len(zlib.compress(raw, 9))
        data = max(
            len(json.dumps(live(s.get(k, {})), ensure_ascii=False, separators=(",", ":")).encode("utf-8"))
            for d, v in demos.items() if v.get("w") == name for s in states[d] for k in ("data", "answer")
        )
        src[name] = html
        sizes[name] = {"html": len(raw), "zip": packed, "data": data}
        if len(raw) > HTML_MAX or packed > ZIP_MAX or data > DATA_MAX or "\0" in html:
            bad.append(name)
        print(f"{name:9} HTML {len(raw):6} / {HTML_MAX}   zlib-9 {packed:5} / {ZIP_MAX}   данные {data:5} / {DATA_MAX}")

    keys = set()
    text = json.dumps(samples, ensure_ascii=False)
    for m in re.finditer(r"\{(-?)([a-z_]+)\}", text):
        keys.add(("mono/" if m.group(1) else "color/") + m.group(2))
    for st_list in states.values():
        for st in st_list:
            for row in st["keys"] + [r for p in st.get("pre", []) for r in p.get("keys", [])]:
                for _, icon, _ in row:
                    if icon:
                        keys.add("color/" + icon[2:] if icon.startswith("c:") else "mono/" + icon)
    for extra in ("color/cb", "color/xr", "color/swap", "color/robot", "color/rates"):
        keys.add(extra)
    emoji = {}
    for k in sorted(keys):
        path = os.path.join(emoji_dir, k + ".json")
        if os.path.exists(path):
            with open(path, encoding="utf-8") as f:
                emoji[k] = json.load(f)
        else:
            print(f"нет эмодзи {k}", file=sys.stderr)

    with open(os.path.join(HERE, "preview.html"), encoding="utf-8") as f:
        page = f.read()
    with open(os.path.join(HERE, "analysis.json"), encoding="utf-8") as f:
        analysis = json.load(f)

    def js(v):
        return json.dumps(v, ensure_ascii=False, separators=(",", ":")).replace("</", "<\\/")

    page = (page.replace("/*WIDGETS*/", js(src)).replace("/*SAMPLES*/", js(samples))
            .replace("/*EMOJI*/", js(emoji)).replace("/*SIZES*/", js(sizes))
            .replace("/*ANALYSIS*/", js(analysis)))
    with open(out, "w", encoding="utf-8") as f:
        f.write(page)
    print(f"{len(emoji)} эмодзи, {len(page.encode()) // 1024} КБ: {out}")
    if bad:
        print("СВЕРХ ЛИМИТА: " + ", ".join(bad), file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
