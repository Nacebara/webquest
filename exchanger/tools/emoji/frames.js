// Эталонные кадры значков из SVG (librsvg через sharp) — для сверки TGS: verify_tgs.py.
//   node tools/emoji/frames.js <папка> [размер]
const fs = require('fs');
const path = require('path');
const sharp = require('sharp');
const { COLOR, MONO, MONO_W } = require('./icons');
const { FRAMES } = require('./tgs');

const OUT = path.resolve(process.argv[2] || 'emoji-frames');
const PX = Number(process.argv[3] || 256);
// Кадры сверки: каждый 7-й из 168 (≈ 8 раз в секунду) — все значки анимированы.
const STEP = Number(process.env.EXCH_STEP || 7);
const FRAME_LIST = Array.from({ length: Math.floor(FRAMES / STEP) }, (_, i) => i * STEP);

const svgColor = (body) =>
  `<svg xmlns="http://www.w3.org/2000/svg" width="${PX}" height="${PX}" viewBox="0 0 24 24">${body}</svg>`;
const svgMono = (body) =>
  `<svg xmlns="http://www.w3.org/2000/svg" width="${PX}" height="${PX}" viewBox="-0.6 -0.6 25.2 25.2"><g fill="none" stroke="#fff" stroke-width="${MONO_W}" stroke-linecap="round" stroke-linejoin="round">${body}</g></svg>`;

async function save(svg, file) {
  await sharp(Buffer.from(svg), { density: 72 }).png().toFile(file);
}

async function main() {
  const index = { step: STEP, frames: FRAMES, mono: [], color: [] };
  for (const [kind, icons, wrap] of [
    ['mono', MONO, svgMono],
    ['color', COLOR, svgColor],
  ]) {
    fs.mkdirSync(path.join(OUT, kind), { recursive: true });
    for (const [key, icon] of Object.entries(icons)) {
      for (const f of FRAME_LIST) await save(wrap(icon(f / FRAMES)), path.join(OUT, kind, `${key}_${f}.png`));
      index[kind].push({ key, frames: FRAME_LIST });
    }
  }
  fs.writeFileSync(path.join(OUT, 'index.json'), JSON.stringify(index));
  console.log(`эталоны: ${OUT}`);
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
