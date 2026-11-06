// Статичная вариация набора и манифест для установки в Telegram.
//   node tools/emoji/static.js assets/emoji
//
// - assets/emoji/static/{color,mono}/<значок>.png — кадр покоя (`t = null` в icons.js), 100×100:
//   так Telegram требует для статичных кастомных эмодзи (формат `static`, PNG);
// - assets/emoji/manifest.json — список значков для `exch emoji install`: вид, подпись,
//   обычный эмодзи (Telegram просит его для каждого кастомного), файлы обеих вариаций.
//
// Цветные значки — плитки, Telegram их не перекрашивает; линейные (`mono`) рисуются белым
// и перекрашиваются в цвет текста (`needs_repainting`) — поэтому у них свой набор.
const fs = require('fs');
const path = require('path');
const sharp = require('sharp');
const { COLOR, MONO, MONO_W, LABELS } = require('./icons');

const OUT = path.resolve(process.argv[2] || 'assets/emoji');
const PX = 100;

const svgColor = (body) =>
  `<svg xmlns="http://www.w3.org/2000/svg" width="${PX}" height="${PX}" viewBox="0 0 24 24">${body}</svg>`;
const svgMono = (body) =>
  `<svg xmlns="http://www.w3.org/2000/svg" width="${PX}" height="${PX}" viewBox="-0.6 -0.6 25.2 25.2"><g fill="none" stroke="#fff" stroke-width="${MONO_W}" stroke-linecap="round" stroke-linejoin="round">${body}</g></svg>`;

// Обычный эмодзи для каждого значка: Telegram показывает его там, где кастомные недоступны,
// и по нему ищет эмодзи в панели.
const BASE = {
  color: {
    done: '✅', swap: '🔄', process: '⏳', wait: '⏳', err: '❌', warn: '⚠️', refund: '↩️',
    manual: '🛠', invoice: '🧾', sending: '📤', usdt: '💵', cb: '🔵', xr: '🚀', shield: '🛡',
    lock: '🔒', rates: '📈', history: '📜', support: '💬', fast: '⚡', pause: '⏸', ban: '⛔',
    alarm: '🚨', live: '🟢', robot: '🤖', vault: '🏦', flood: '🌊', kill: '🛑', maint: '🛠',
    broadcast: '📢', limit: '📏', confetti: '🎉', step_done: '✅', step_now: '🔵', step_todo: '⚪',
    dot_ok: '🟢', dot_work: '🔵', dot_wait: '🟡', dot_act: '🔴', dot_closed: '⚫',
    prog_25: '🕒', prog_50: '🕕', prog_75: '🕘',
  },
  mono: {
    swap: '🔄', ticket: '🎫', invoice: '🧾', rates: '📈', history: '📜', support: '💬', help: '❓',
    copy: '📋', claim: '📥', cb: '🔵', xr: '🚀', usdt: '💵', done: '✅', close: '❌', back: '◀️',
    home: '🏠', refresh: '🔄', refund: '↩️', wait: '⏳', spin: '🌀', fast: '⚡', gift: '🎁',
    star: '⭐', globe: '🌐', link: '🔗', dash: '📊', queue: '📋', order: '📄', user: '👤',
    users: '👥', ban: '🚫', search: '🔍', filter: '🔽', settings: '⚙️', bell: '🔔', vault: '🏦',
    robot: '🤖', megaphone: '📣', journal: '📓', chart: '📊', export: '📤', emoji: '😀',
    staff: '👔', key: '🔑', power: '🛑', pause: '⏸', play: '▶️', maint: '🛠', shield: '🛡',
    lock: '🔒', eye: '👁', edit: '✏️', plus: '➕', minus: '➖', trash: '🗑', calendar: '📅',
    limit: '📏', alert: '⚠️', flag: '🚩', pin: '📌', topic: '💭', ack: '👍', take: '✋',
    coin: '🪙', sparkle: '✨', phone: '📱', plug: '🔌', inbox: '📥', server: '🖥', wallet: '👛',
  },
};

async function main() {
  const manifest = { size: PX, icons: [] };
  for (const [kind, icons, wrap] of [
    ['color', COLOR, svgColor],
    ['mono', MONO, svgMono],
  ]) {
    const dir = path.join(OUT, 'static', kind);
    fs.mkdirSync(dir, { recursive: true });
    for (const [key, icon] of Object.entries(icons)) {
      const base = BASE[kind][key];
      if (!base) throw new Error(`нет обычного эмодзи для ${kind}/${key}`);
      if (!LABELS[key]) throw new Error(`нет подписи для ${key}`);
      const file = path.join(dir, `${key}.png`);
      await sharp(Buffer.from(wrap(icon(null))), { density: 72 }).png({ compressionLevel: 9 }).toFile(file);
      const meta = await sharp(file).metadata();
      if (meta.width !== PX || meta.height !== PX) throw new Error(`${file}: ${meta.width}×${meta.height}`);
      manifest.icons.push({
        key: `${kind}/${key}`,
        label: LABELS[key],
        emoji: base,
        animated: `${kind}/${key}.tgs`,
        static: `static/${kind}/${key}.png`,
      });
    }
  }
  fs.writeFileSync(path.join(OUT, 'manifest.json'), `${JSON.stringify(manifest, null, 1)}\n`);
  console.log(`статичные значки и манифест: ${OUT} (${manifest.icons.length})`);
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
