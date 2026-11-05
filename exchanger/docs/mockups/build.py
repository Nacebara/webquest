"""Сборка страницы макетов: шаблон + Lottie JSON всех эмодзи + подписи из icons.js.

    EXCH_JSON=1 node tools/emoji/tgs.js /tmp/emoji        # рядом с .tgs появятся .json
    python3 docs/mockups/build.py /tmp/emoji /tmp/exch-mockups.html
"""

import json
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ICONS = os.path.join(HERE, "..", "..", "tools", "emoji", "icons.js")


def main():
    src, out = sys.argv[1], sys.argv[2]
    emoji = {}
    for kind in ("color", "mono"):
        folder = os.path.join(src, kind)
        for name in sorted(os.listdir(folder)):
            if name.endswith(".json"):
                with open(os.path.join(folder, name), encoding="utf-8") as f:
                    emoji[f"{kind}/{name[:-5]}"] = json.load(f)
    labels = json.loads(subprocess.check_output(
        ["node", "-e", "process.stdout.write(JSON.stringify(require(process.argv[1]).LABELS))", ICONS]))
    with open(os.path.join(HERE, "template.html"), encoding="utf-8") as f:
        page = f.read()
    page = page.replace("/*EMOJI*/", json.dumps(emoji, separators=(",", ":"), ensure_ascii=False).replace("</", "<\\/"))
    page = page.replace("/*LABELS*/", json.dumps(labels, ensure_ascii=False))
    with open(out, "w", encoding="utf-8") as f:
        f.write(page)
    print(f"{len(emoji)} эмодзи, {len(page.encode()) // 1024} КБ: {out}")


if __name__ == "__main__":
    main()
