// Быстрые проверки описаний значков (до сборки TGS):
// • петля бесшовна: кадры t = 0 и t = 1 совпадают по пикселям;
// • жест не теряется: значок действительно двигается (кадры петли различаются);
// • у каждого значка есть подпись для превью и админки;
// • в описаниях нет того, что конвертер или Telegram не примут (text, image, mask, filter).
//   node tools/emoji/check.js
const sharp = require('sharp');
const { COLOR, MONO, LABELS, MONO_W } = require('./icons');

const PX = 96;
const wrap = {
  color: (body) => `<svg xmlns="http://www.w3.org/2000/svg" width="${PX}" height="${PX}" viewBox="0 0 24 24">${body}</svg>`,
  mono: (body) =>
    `<svg xmlns="http://www.w3.org/2000/svg" width="${PX}" height="${PX}" viewBox="-0.6 -0.6 25.2 25.2"><g fill="none" stroke="#fff" stroke-width="${MONO_W}" stroke-linecap="round" stroke-linejoin="round">${body}</g></svg>`,
};
const raw = (kind, body) => sharp(Buffer.from(wrap[kind](body))).ensureAlpha().raw().toBuffer();
const diff = (a, b) => {
  let sum = 0;
  for (let i = 0; i < a.length; i++) sum += Math.abs(a[i] - b[i]);
  return sum / a.length;
};

async function main() {
  const errors = [];
  for (const [kind, icons] of [
    ['color', COLOR],
    ['mono', MONO],
  ]) {
    for (const [key, icon] of Object.entries(icons)) {
      const name = `${kind}/${key}`;
      if (!LABELS[key]) errors.push(`${name}: нет подписи в LABELS`);
      const times = [null, 0, 0.1, 0.2, 0.3, 0.45, 0.81, 1];
      if (times.some((t) => /<(text|image|mask|filter|foreignObject)\b/.test(icon(t)))) errors.push(`${name}: недопустимый элемент`);
      const probes = Array.from({ length: 12 }, (_, i) => 0.05 * (i + 1)); // жест — в первой половине петли
      const [first, last, ...mid] = await Promise.all([0, 1, ...probes].map((t) => raw(kind, icon(t))));
      const seam = diff(first, last);
      if (seam > 0.5) errors.push(`${name}: кадры t=0 и t=1 различаются (${seam.toFixed(2)}) — петля со скачком`);
      if (Math.max(...mid.map((m) => diff(first, m))) < 0.3) errors.push(`${name}: не двигается — нужен жест`);
    }
  }
  if (errors.length) {
    console.error(errors.join('\n'));
    process.exit(1);
  }
  console.log(`ок: ${Object.keys(COLOR).length} цветных и ${Object.keys(MONO).length} моно, все анимированы, петли бесшовны`);
}
main();
