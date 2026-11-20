"""Страница макетов с новой группой «на утверждение» — для картинки владельцу.

    EXCH_JSON=1 node tools/emoji/tgs.js /tmp/emoji            # премиум-эмодзи из исходников
    npm pack lottie-web@5.12.2 && tar -xzf lottie-web-5.12.2.tgz   # та же версия, что в макетах
    python3 -I tools/mockups/proposal.py docs/mockups/widgets/<группа>.js /tmp/emoji \
        package/build/player/lottie_light.min.js /tmp/page.html
    node tools/mockups/shot.js /tmp/page.html docs/mockups/widgets/<группа>.png

Группа встаёт первой во вкладке «Пульт» (как в утверждённой странице), lottie встраивается в
страницу: снимок не зависит от сети. Эмодзи — все из tools/emoji, иначе отказ.
"""

import json
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.normpath(os.path.join(HERE, '..', '..'))
CDN = '<script src="https://cdnjs.cloudflare.com/ajax/libs/lottie-web/5.12.2/lottie_light.min.js"></script>'


def main():
    group_js, emoji_dir, lottie_js, out = sys.argv[1:5]
    tpl = open(os.path.join(REPO, 'docs/mockups/template.html'), encoding='utf-8').read()
    anchor = 'const ADMIN = [\n'
    if tpl.count(anchor) != 1 or tpl.count(CDN) != 1:
        sys.exit('template.html изменился: нет места для группы или подключения lottie')
    tpl = tpl.replace(anchor, anchor + open(group_js, encoding='utf-8').read(), 1)
    lottie = open(lottie_js, encoding='utf-8').read()
    tpl = tpl.replace(CDN, '<script>' + lottie.replace('</', '<\\/') + '</script>', 1)
    icons = os.path.join(REPO, 'tools/emoji/icons.js')
    sets = json.loads(subprocess.check_output(['node', '-e', 'const m=require(process.argv[1]);process.stdout.write(JSON.stringify({color:Object.keys(m.COLOR),mono:Object.keys(m.MONO),labels:m.LABELS}))', icons]))
    emoji = {}
    for kind in ('color', 'mono'):
        for name in sets[kind]:
            path = os.path.join(emoji_dir, kind, name + '.json')
            if not os.path.isfile(path):
                sys.exit(f'нет эмодзи {kind}/{name}: соберите заново tools/emoji/tgs.js')
            emoji[f'{kind}/{name}'] = json.load(open(path, encoding='utf-8'))
    tpl = tpl.replace('/*EMOJI*/', json.dumps(emoji, separators=(',', ':'), ensure_ascii=False).replace('</', '<\\/'))
    tpl = tpl.replace('/*LABELS*/', json.dumps(sets['labels'], ensure_ascii=False))
    open(out, 'w', encoding='utf-8').write(tpl)
    print(f'{len(emoji)} эмодзи, {len(tpl.encode()) // 1024} КБ: {out}')


if __name__ == '__main__':
    main()
