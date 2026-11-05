// Быстрые проверки описаний значков (до сборки TGS):
// • петля бесшовна: у анимированного значка кадры t=0 и t=1 совпадают по пикселям (поворот на
//   720° и «ещё не нарисовано» / «уже растаяло» — одно и то же изображение);
// • у каждого значка есть подпись для превью и админки;
// • в описаниях нет того, что конвертер или Telegram не примут (text, image, mask, filter).
//   node tools/emoji/check.js
const sharp = require('sharp');
const { COLOR, MONO, LABELS } = require('./icons');

const PX = 96;
const svg = (body) =>
  Buffer.from(`<svg xmlns="http://www.w3.org/2000/svg" width="${PX}" height="${PX}" viewBox="0 0 24 24">${body}</svg>`);
const raw = (body) => sharp(svg(body)).ensureAlpha().raw().toBuffer();

async function main() {
  const errors = [];
  for (const [key, icon] of Object.entries(COLOR)) {
    if (!LABELS[key]) errors.push(`color/${key}: нет подписи в LABELS`);
    const times = icon.anim ? [null, 0, 0.37, 0.81, 1] : [null];
    for (const t of times) {
      if (/<(text|image|mask|filter|foreignObject)\b/.test(icon.svg(t))) errors.push(`color/${key}: недопустимый элемент`);
    }
    if (icon.anim) {
      const [a, b] = await Promise.all([raw(icon.svg(0)), raw(icon.svg(1))]);
      let sum = 0;
      for (let i = 0; i < a.length; i++) sum += Math.abs(a[i] - b[i]);
      const mean = sum / a.length;
      if (mean > 0.5) errors.push(`color/${key}: кадры t=0 и t=1 различаются (${mean.toFixed(2)}) — петля со скачком`);
    }
  }
  for (const [key, body] of Object.entries(MONO)) {
    if (!LABELS[key]) errors.push(`mono/${key}: нет подписи в LABELS`);
    if (/<(text|image|mask|filter|foreignObject)\b/.test(body)) errors.push(`mono/${key}: недопустимый элемент`);
  }
  if (errors.length) {
    console.error(errors.join('\n'));
    process.exit(1);
  }
  const anim = Object.values(COLOR).filter((i) => i.anim).length;
  console.log(`ок: ${Object.keys(COLOR).length} цветных (${anim} анимированы, петли бесшовны), ${Object.keys(MONO).length} моно`);
}
main();
