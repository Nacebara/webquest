// Эталонные кадры значков из SVG (librsvg через sharp) — для сверки TGS: verify_tgs.py.
//   node tools/emoji/frames.js <папка> [размер]
const fs = require('fs');
const path = require('path');
const sharp = require('sharp');
const { COLOR, MONO } = require('./icons');
const { FRAMES } = require('./tgs');

const OUT = path.resolve(process.argv[2] || 'emoji-frames');
const PX = Number(process.argv[3] || 256);
// Кадры сверки анимаций: каждый 7-й из 168 (≈ 8 раз в секунду).
const STEP = Number(process.env.EXCH_STEP || 7);

const svgColor = (body) =>
  `<svg xmlns="http://www.w3.org/2000/svg" width="${PX}" height="${PX}" viewBox="0 0 24 24">${body}</svg>`;
const svgMono = (body) =>
  `<svg xmlns="http://www.w3.org/2000/svg" width="${PX}" height="${PX}" viewBox="-0.6 -0.6 25.2 25.2"><g fill="none" stroke="#fff" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">${body}</g></svg>`;

async function save(svg, file) {
  await sharp(Buffer.from(svg), { density: 72 }).png().toFile(file);
}

async function main() {
  for (const d of ['mono', 'color']) fs.mkdirSync(path.join(OUT, d), { recursive: true });
  const index = { step: STEP, frames: FRAMES, mono: [], color: [] };
  for (const [key, body] of Object.entries(MONO)) {
    await save(svgMono(body), path.join(OUT, 'mono', `${key}_0.png`));
    index.mono.push({ key, frames: [0] });
  }
  for (const [key, icon] of Object.entries(COLOR)) {
    const frames = icon.anim ? Array.from({ length: Math.floor(FRAMES / STEP) }, (_, i) => i * STEP) : [0];
    for (const f of frames) {
      await save(svgColor(icon.svg(icon.anim ? f / FRAMES : null)), path.join(OUT, 'color', `${key}_${f}.png`));
    }
    index.color.push({ key, frames });
  }
  fs.writeFileSync(path.join(OUT, 'index.json'), JSON.stringify(index));
  console.log(`эталоны: ${OUT}`);
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
