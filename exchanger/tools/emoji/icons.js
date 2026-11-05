// Премиум-эмодзи exch: одна семья значков на сетке 24×24, из неё tgs.js собирает два набора
// кастомных эмодзи Telegram (TGS — вектор Lottie, 512×512, 60 кадров/с).
//
// • COLOR — плитки-«сквиркалы» с диагональным градиентом и стеклянным бликом сверху, белый знак;
//   точки статусов, шаги, сегменты шкал, монета USDT, знаки кошельков. Не перекрашиваются.
// • MONO — линия 2, белая; набор ставится с needs_repainting: Telegram красит его в цвет текста
//   или кнопки. На кнопках бот всегда ставит моно.
//
// Значок — функция (t) => SVG: t — время кадра 0..1 для анимированных, null — статичный кадр.
// Анимации спокойные: один короткий акцент за петлю (2,8 с), первый и последний кадр совпадают.
//
// Отличия от «Реборна» (откуда взят конвертер): своя форма (плитка, а не круг), контраст белого
// знака не ниже 3:1 в центре плитки (янтарный и зелёный темнее), линии знаков не тоньше 1,75
// единицы после масштаба (на 20 px не мерцают), нейтральные элементы светлее — читаются на обеих
// темах.

const C = {
  mint: '#12A26C',
  amber: '#CC7300',
  blue: '#2F6BEA',
  coral: '#E0443C',
  violet: '#6A4CF0',
  slate: '#5E6D7F',
  sky: '#1C97E6',
  night: '#283050',
  coin: '#0E9E7B',
  coinRim: '#0A7A60',
  grey: '#8A98A8',
};
const W = '#FFFFFF';

const clamp = (v, a = 0, b = 1) => Math.max(a, Math.min(b, v));
const ease = (k) => (k < 0.5 ? 2 * k * k : 1 - Math.pow(-2 * k + 2, 2) / 2);
const easeOut = (k) => 1 - Math.pow(1 - k, 3);
// С перелётом и возвратом: пружинка в конце.
const easeBack = (k) => {
  const c1 = 1.55;
  const c3 = c1 + 1;
  return 1 + c3 * Math.pow(k - 1, 3) + c1 * Math.pow(k - 1, 2);
};
// 0 → 1 → 0 на отрезке [a, b] цикла.
const bump = (t, a, b) => Math.sin(Math.PI * clamp((t - a) / (b - a)));
const seg = (t, a, b) => clamp((t - a) / (b - a));
const n = (v) => Number(v.toFixed(3));

// Светлее или темнее на долю k: k > 0 — к белому, k < 0 — к чёрному.
const shade = (hex, k) => {
  const to = k > 0 ? 255 : 0;
  return `#${[1, 3, 5]
    .map((i) => parseInt(hex.slice(i, i + 2), 16))
    .map((x) => Math.round(x + (to - x) * Math.abs(k)).toString(16).padStart(2, '0'))
    .join('')}`;
};

// ── Плитка ─────────────────────────────────────────────────────────────────────────────
const TILE = { x: 1.4, y: 1.4, s: 21.2, r: 6.6 };
const GLYPH_SCALE = 0.84;
const GLYPH_W = 2.1; // после масштаба 0,84 → 1,76

const tileRect = (extra = '') =>
  `<rect x="${TILE.x}" y="${TILE.y}" width="${TILE.s}" height="${TILE.s}" rx="${TILE.r}"${extra}/>`;

// Плитка с диагональным градиентом (сверху слева светлее, снизу справа глубже) и стеклом.
const tileBg = (id, color) =>
  `<defs><linearGradient id="${id}g" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="${shade(color, 0.14)}"/><stop offset="1" stop-color="${shade(color, -0.1)}"/></linearGradient>` +
  `<linearGradient id="${id}h" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#fff" stop-opacity=".16"/><stop offset=".7" stop-color="#fff" stop-opacity="0"/></linearGradient></defs>` +
  tileRect(` fill="url(#${id}g)"`) +
  tileRect(` fill="url(#${id}h)"`);

// Белый знак; масштаб — от центра значка, после собственного преобразования знака.
const glyph = (body, opts = {}) => {
  const tr = [opts.transform, `translate(12 12) scale(${opts.scale || GLYPH_SCALE}) translate(-12 -12)`]
    .filter(Boolean)
    .join(' ');
  return `<g fill="none" stroke="${W}" stroke-width="${opts.sw || GLYPH_W}" stroke-linecap="round" stroke-linejoin="round" transform="${tr}"${
    opts.opacity !== undefined ? ` opacity="${n(opts.opacity)}"` : ''
  }>${body}</g>`;
};

const tile = (id, color, body, opts = {}) =>
  `${opts.wrap ? `<g transform="${opts.wrap}">` : ''}${tileBg(id, color)}${opts.under || ''}${glyph(body, opts)}${opts.over || ''}${opts.wrap ? '</g>' : ''}`;

// Блик по плитке: светлая наклонная полоса проходит слева направо; k — 0..1.
const sheen = (id, k, strength = 0.34) => {
  if (k <= 0 || k >= 1) return '';
  const x = n(-10 + 34 * k);
  return `<defs><clipPath id="${id}c">${tileRect()}</clipPath><linearGradient id="${id}s" x1="0" y1="0" x2="1" y2="0"><stop offset="0" stop-color="#fff" stop-opacity="0"/><stop offset=".5" stop-color="#fff" stop-opacity="${strength}"/><stop offset="1" stop-color="#fff" stop-opacity="0"/></linearGradient></defs><g clip-path="url(#${id}c)"><rect x="${x}" y="-8" width="6" height="40" fill="url(#${id}s)" transform="rotate(20 12 12)"/></g>`;
};

// Четырёхлучевая искра.
const SPARK = (cx, cy, r) => {
  const k = r * 0.18;
  return `M${n(cx)} ${n(cy - r)}C${n(cx + k)} ${n(cy - k)} ${n(cx + k)} ${n(cy - k)} ${n(cx + r)} ${n(cy)}C${n(cx + k)} ${n(cy + k)} ${n(cx + k)} ${n(cy + k)} ${n(cx)} ${n(cy + r)}C${n(cx - k)} ${n(cy + k)} ${n(cx - k)} ${n(cy + k)} ${n(cx - r)} ${n(cy)}C${n(cx - k)} ${n(cy - k)} ${n(cx - k)} ${n(cy - k)} ${n(cx)} ${n(cy - r)}Z`;
};

// Прорисовка линии: путь длиной len виден на долю k (trim paths в TGS).
const drawOn = (d, len, k, extra = '') => {
  if (k <= 0.001) return '';
  if (k >= 0.999) return `<path d="${d}"${extra}/>`;
  return `<path d="${d}" stroke-dasharray="${n(len * k)} ${n(len * 2)}"${extra}/>`;
};

// ── Знаки (в координатах 24×24 до масштаба плитки) ───────────────────────────────────
const G = {
  check: 'M7 12.4l3.3 3.3L17 9',
  cross1: 'M8 8l8 8',
  cross2: 'M16 8l-8 8',
  excl: `<path d="M12 6.6v6.6"/><circle cx="12" cy="17" r="1.45" fill="${W}" stroke="none"/>`,
  // Обмен: две встречные стрелки, центрально-симметричны — поворот на 180° даёт тот же рисунок.
  swap: '<path d="M5.6 9h12.2M14.8 5.9l3.1 3.1-3.1 3.1"/><path d="M18.4 15H6.2M9.2 11.9L6.1 15l3.1 3.1"/>',
  // Чек: билет с вырезами по бокам и перфорацией.
  ticket:
    '<path d="M4.6 8.6a1.6 1.6 0 0 1 1.6-1.6h11.6a1.6 1.6 0 0 1 1.6 1.6v1.5a1.9 1.9 0 0 0 0 3.8v1.5a1.6 1.6 0 0 1-1.6 1.6H6.2a1.6 1.6 0 0 1-1.6-1.6v-1.5a1.9 1.9 0 0 0 0-3.8z"/><path d="M14.4 8.9v6.2" stroke-dasharray="1.3 1.75"/>',
  receipt: 'M7.2 5h9.6v14l-1.6-1.1-1.6 1.1-1.6-1.1-1.6 1.1-1.6-1.1-1.6 1.1z',
  receiptLines: ['M9.8 8.8h4.4', 'M9.8 11.9h4.4', 'M9.8 15h2.7'],
  // Возврат: путь от хвоста к острию — так он «прорисовывается» к стрелке.
  uturn: 'M11.2 18.4h3.2a4.2 4.2 0 0 0 0-8.4H6.2',
  uturnHead: 'M9.6 6.6L6.2 10l3.4 3.4',
  clockFace: '<circle cx="12" cy="12" r="7.1"/>',
  shield: 'M12 3.9l6.6 2.4v5c0 4.2-2.8 7.1-6.6 8.7-3.8-1.6-6.6-4.5-6.6-8.7v-5z',
  lock: '<rect x="6.6" y="10.8" width="10.8" height="8.2" rx="2.2"/><path d="M8.9 10.8V8.6a3.1 3.1 0 0 1 6.2 0v2.2"/>',
  percent: `<path d="M16.6 7.4l-9.2 9.2"/><circle cx="8.4" cy="8.4" r="1.75"/><circle cx="15.6" cy="15.6" r="1.75"/>`,
  history: '<path d="M5.6 12a6.4 6.4 0 1 0 1.95-4.6"/><path d="M5.3 4.6v3.3h3.3"/><path d="M12 8.8v3.6l2.4 1.5"/>',
  chat: '<path d="M5.6 8.2a2.4 2.4 0 0 1 2.4-2.4h8a2.4 2.4 0 0 1 2.4 2.4v5.4a2.4 2.4 0 0 1-2.4 2.4h-4.4l-3.7 2.9V16a2.3 2.3 0 0 1-2.3-2.4z"/>',
  bolt: `<path d="M13.2 4.4L7.4 13.1h4.8l-1.3 6.5 5.7-8.6h-4.7z" fill="${W}" stroke-width="1.2"/>`,
  pause: '<path d="M9.6 7.6v8.8M14.4 7.6v8.8"/>',
  power: '<path d="M12 4.8v6.4"/><path d="M8 7.4a6.2 6.2 0 1 0 8 0"/>',
  robot:
    `<rect x="6.2" y="8.6" width="11.6" height="9" rx="2.8"/><path d="M12 8.6V6"/><circle cx="12" cy="5.4" r=".9" fill="${W}" stroke="none"/><circle cx="9.7" cy="13" r="1.25" fill="${W}" stroke="none"/><circle cx="14.3" cy="13" r="1.25" fill="${W}" stroke="none"/>`,
  vault: '<rect x="5.4" y="5.8" width="13.2" height="12.4" rx="2.4"/><circle cx="12" cy="12" r="2.8"/><path d="M12 7.6v1.6M12 14.8v1.6M7.6 12h1.6M14.8 12h1.6"/>',
  hourglass: '<path d="M8.4 5.6h7.2M8.4 18.4h7.2M9.4 5.7v1.6c0 1.7 2.6 2.9 2.6 4.7s-2.6 3-2.6 4.7v1.6M14.6 5.7v1.6c0 1.7-2.6 2.9-2.6 4.7s2.6 3 2.6 4.7v1.6"/>',
  // Знак CryptoBot в exch — «орбита»: дуга с точкой в просвете (не логотип кошелька).
  orbit: `<path d="M16.7 8.3a6.1 6.1 0 1 0 0 7.4"/><circle cx="17.3" cy="12" r="1.55" fill="${W}" stroke="none"/>`,
  // Знак xRocket в exch — ракета (не логотип кошелька), строится вверх и поворачивается на 45°.
  rocket:
    '<path d="M12 4.4c2.5 1.9 3.7 4.6 3.7 7.6v3H8.3v-3c0-3 1.2-5.7 3.7-7.6z"/><circle cx="12" cy="10.6" r="1.35"/><path d="M8.3 12.9l-1.9 1.8V17h1.9M15.7 12.9l1.9 1.8V17h-1.9M12 17.4v1.8"/>',
  magnifier: '<circle cx="10.6" cy="10.6" r="3.9"/><path d="M13.5 13.5l3.2 3.2"/>',
  docLines: '<path d="M6 7.6h12M6 12h12M6 16.4h7.4"/>',
  wrench: '<path d="M14.6 6.1a3.9 3.9 0 0 0-4.7 5l-4.2 4.2a1.6 1.6 0 0 0 2.3 2.3l4.2-4.2a3.9 3.9 0 0 0 5-4.7l-2.3 2.3-2.4-.6-.6-2.4z"/>',
  ban: '<circle cx="12" cy="12" r="6.6"/><path d="M7.4 7.4l9.2 9.2"/>',
  megaphone: '<path d="M6 10.4v3.2a1 1 0 0 0 1 1h2.2l5.6 3.4V6l-5.6 3.4H7a1 1 0 0 0-1 1z"/><path d="M17.6 9.6a3.4 3.4 0 0 1 0 4.8"/>',
  gauge: '<path d="M5.4 15.6a6.8 6.8 0 1 1 13.2 0"/><path d="M12 13.4l3-3.6"/><circle cx="12" cy="13.6" r=".9" fill="#fff" stroke="none"/>',
};

const L = { receiptLine: 4.4 };

// Сегмент шкалы — скруглённая полоса во всю ширину значка: промежутки между эмодзи — деления.
const SEG_X = 1.2;
const SEG_Y = 8.5;
const SEG_W = 21.6;
const SEG_H = 7;
const SEGR = (color, opacity = 1) =>
  `<rect x="${SEG_X}" y="${SEG_Y}" width="${SEG_W}" height="${SEG_H}" rx="${SEG_H / 2}" fill="${color}"${opacity < 1 ? ` fill-opacity="${opacity}"` : ''}/>`;
const SEG = (id, color) =>
  `<defs><linearGradient id="${id}" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="${shade(color, 0.14)}"/><stop offset="1" stop-color="${shade(color, -0.08)}"/></linearGradient></defs>` +
  `<rect x="${SEG_X}" y="${SEG_Y}" width="${SEG_W}" height="${SEG_H}" rx="${SEG_H / 2}" fill="url(#${id})"/>`;

// Квадратная «волна»: скруглённый квадрат выходит из-под плитки, расходится и истончается.
// Не прозрачностью: полупрозрачный цвет на тёмной теме выглядит грязным пятном, а тонкая
// линия полного цвета чистая на обеих темах.
const ripple = (color, k, base = 5, grow = 5.6) => {
  if (k <= 0.001 || k >= 0.999) return '';
  const s = base + grow * easeOut(k);
  const w = 1.9 * Math.pow(1 - k, 1.25);
  if (w < 0.06) return '';
  return `<rect x="${n(12 - s)}" y="${n(12 - s)}" width="${n(2 * s)}" height="${n(2 * s)}" rx="${n(s * 0.42)}" fill="none" stroke="${color}" stroke-width="${n(w)}"/>`;
};

const coin = (id, sheenK = 0) => {
  const sh =
    sheenK > 0 && sheenK < 1
      ? `<defs><clipPath id="${id}c"><circle cx="12" cy="12" r="10.4"/></clipPath><linearGradient id="${id}s" x1="0" y1="0" x2="1" y2="0"><stop offset="0" stop-color="#fff" stop-opacity="0"/><stop offset=".5" stop-color="#fff" stop-opacity=".4"/><stop offset="1" stop-color="#fff" stop-opacity="0"/></linearGradient></defs><g clip-path="url(#${id}c)"><rect x="${n(-10 + 34 * sheenK)}" y="-8" width="5.5" height="40" fill="url(#${id}s)" transform="rotate(20 12 12)"/></g>`
      : '';
  return (
    `<defs><linearGradient id="${id}g" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="${shade(C.coin, 0.16)}"/><stop offset="1" stop-color="${shade(C.coin, -0.08)}"/></linearGradient></defs>` +
    `<circle cx="12" cy="12" r="10.6" fill="${C.coinRim}"/><circle cx="12" cy="12" r="9.1" fill="url(#${id}g)"/>` +
    `<circle cx="12" cy="12" r="7.6" fill="none" stroke="${W}" stroke-opacity=".3" stroke-width="1.1"/>` +
    sh +
    glyph(
      '<path d="M14.7 9.2c-.5-1-1.5-1.6-2.7-1.6-1.6 0-2.7.8-2.7 2 0 2.8 5.6 1.6 5.6 4.5 0 1.3-1.2 2.2-2.9 2.2-1.4 0-2.4-.6-2.8-1.6M12 6v1.6M12 16.3v1.7"/>',
      { scale: 0.86, sw: 2.2 },
    )
  );
};

// ── Цветные значки ──────────────────────────────────────────────────────────────────
const COLOR = {
  // Готово: галочка мягко «подпрыгивает», в углу вспыхивает искра. Один слой без матта: при
  // сведении слоёв rlottie (8 бит на слой) плитка дала бы ровный сдвиг цвета.
  done: {
    anim: true,
    svg: (t) => {
      const pop = t == null ? 1 : 1 + 0.11 * bump(t, 0.06, 0.26) - 0.03 * bump(t, 0.26, 0.38);
      const tw = t == null ? 0 : bump(t, 0.22, 0.56);
      const spark = tw > 0.02 ? `<path d="${SPARK(18, 6.1, 2.7 * tw)}" fill="${W}" opacity="${n(0.92 * tw)}"/>` : '';
      const scale = Math.abs(pop - 1) > 0.0005 ? `translate(12 12) scale(${n(pop)}) translate(-12 -12)` : '';
      return tile('dn', C.mint, `<path d="${G.check}"/>`, { transform: scale, over: spark });
    },
  },
  // Обмен — фирменный значок: стрелки делают пол-оборота с пружинкой и стоят.
  swap: {
    anim: true,
    svg: (t) => {
      const a = t == null ? 0 : 180 * easeBack(seg(t, 0.08, 0.5));
      // На 0° и ровно 180° рисунок один и тот же; перелёт пружинки (больше 180°) — поворот.
      const still = Math.abs(a) < 0.01 || Math.abs(a - 180) < 0.01;
      return tile('sw', C.blue, G.swap, { transform: still ? '' : `rotate(${n(a)} 12 12)` });
    },
  },
  // В работе: две дуги орбиты вокруг ядра, ровно два оборота за петлю.
  process: {
    anim: true,
    svg: (t) => {
      const a = t == null ? 0 : 720 * t;
      const s = t == null ? 1 : 1 - 0.12 * Math.sin(2 * Math.PI * 2 * t);
      const len = 2 * Math.PI * 8.2;
      return (
        `<circle cx="12" cy="12" r="8.2" fill="none" stroke="${C.blue}" stroke-opacity=".22" stroke-width="2.4"/>` +
        `<g transform="rotate(${n(a - 90)} 12 12)"><circle cx="12" cy="12" r="8.2" fill="none" stroke="${C.blue}" stroke-width="2.4" stroke-linecap="round" stroke-dasharray="${n(len * 0.24)} ${n(len * 0.26)}"/></g>` +
        `<circle cx="12" cy="12" r="${n(3.1 * s)}" fill="${C.blue}"/>`
      );
    },
  },
  // Ждём: минутная стрелка делает оборот за петлю.
  wait: {
    anim: true,
    svg: (t) => {
      const a = t == null ? 0 : 360 * t;
      return tile(
        'wt',
        C.amber,
        `${G.clockFace}<path d="M12 12l2.9 1.7"/><g transform="rotate(${n(a)} 12 12)"><path d="M12 12V7.4"/></g>`,
      );
    },
  },
  // Ошибка: крест на коралловой плитке коротко вздрагивает; плитка стоит на месте.
  err: {
    anim: true,
    svg: (t) => {
      const k = t == null ? 1 : seg(t, 0.06, 0.36);
      const dx = t == null ? 0 : 1.1 * Math.sin(2 * Math.PI * 3 * k) * (1 - k);
      return tile('er', C.coral, `<path d="${G.cross1}"/><path d="${G.cross2}"/>`, {
        transform: Math.abs(dx) > 0.001 ? `translate(${n(dx)} 0)` : '',
        sw: 2.4,
      });
    },
  },
  warn: { svg: () => tile('wn', C.amber, G.excl, { sw: 2.4 }) },
  // Возврат: стрелка дважды мягко «тянется» назад — деньги возвращаются тем же путём.
  refund: {
    anim: true,
    svg: (t) => {
      const dx = t == null ? 0 : -1.5 * bump(t, 0.06, 0.28) - 0.8 * bump(t, 0.28, 0.44);
      return tile('rf', C.violet, `<path d="${G.uturn}"/><path d="${G.uturnHead}"/>`, {
        transform: Math.abs(dx) > 0.001 ? `translate(${n(dx)} 0)` : '',
      });
    },
  },
  // Ручная проверка: лупа неторопливо скользит по строкам.
  manual: {
    anim: true,
    svg: (t) => {
      const dx = t == null ? 0 : 2.6 * Math.sin(2 * Math.PI * t);
      const dy = t == null ? 0 : 1.2 * Math.sin(4 * Math.PI * t);
      return tile('mn', C.violet, `<g opacity=".5">${G.docLines}</g><g transform="translate(${n(dx)} ${n(dy)})">${G.magnifier}</g>`);
    },
  },
  // Счёт: строки чека «печатаются» по одной, держатся и тают к концу петли.
  invoice: {
    anim: true,
    svg: (t) => {
      const fade = t == null ? 1 : 1 - seg(t, 0.86, 0.98);
      const lines = G.receiptLines
        .map((d, i) => {
          if (t == null) return `<path d="${d}"/>`;
          const k = ease(seg(t, 0.08 + i * 0.12, 0.2 + i * 0.12));
          const len = i === 2 ? 2.7 : L.receiptLine;
          return drawOn(d, len, k);
        })
        .join('');
      return tile('iv', C.sky, `<path d="${G.receipt}"/><g opacity="${n(fade)}">${lines}</g>`);
    },
  },
  // Чек в пути: билет плывёт, полосы скорости уплывают назад.
  sending: {
    anim: true,
    svg: (t) => {
      const lines = [[8.6, 3.2], [12, 4.4], [15.4, 3]]
        .map(([y, len], i) => {
          if (t == null) return `<path d="M${n(7.2 - len)} ${y}h${len}" stroke="${C.mint}" stroke-opacity=".6" stroke-width="1.8" stroke-linecap="round"/>`;
          const ph = (t * 3 + i * 0.33) % 1;
          // Скруглённые концы (0,9) не выходят за левый край холста.
          const x = 8.2 - len - 2.4 * ph;
          const o = 0.8 * Math.sin(Math.PI * ph);
          return `<path d="M${n(x)} ${y}h${len}" stroke="${C.mint}" stroke-opacity="${n(o)}" stroke-width="1.8" stroke-linecap="round"/>`;
        })
        .join('');
      const dy = t == null ? 0 : 0.55 * Math.sin(2 * Math.PI * 3 * t);
      const body =
        `<defs><linearGradient id="sdg" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="${shade(C.mint, 0.14)}"/><stop offset="1" stop-color="${shade(C.mint, -0.1)}"/></linearGradient></defs>` +
        '<path d="M8.4 8.2a1.4 1.4 0 0 1 1.4-1.4h10.4a1.4 1.4 0 0 1 1.4 1.4v1.5a2.3 2.3 0 0 0 0 4.6v1.5a1.4 1.4 0 0 1-1.4 1.4H9.8a1.4 1.4 0 0 1-1.4-1.4v-1.5a2.3 2.3 0 0 0 0-4.6z" fill="url(#sdg)"/>' +
        `<path d="M17.2 8.9v6.2" fill="none" stroke="${W}" stroke-width="1.6" stroke-linecap="round" stroke-dasharray="1.3 1.75"/>`;
      return `${lines}<g transform="translate(0 ${n(dy)})">${body}</g>`;
    },
  },
  // Монета USDT: своя (не логотип); раз за петлю по ней проходит блик.
  usdt: {
    anim: true,
    svg: (t) => coin('cn', t == null ? 0 : ease(seg(t, 0.1, 0.55))),
  },
  // Знаки кошельков в exch — свои рисунки на фирменных плитках.
  cb: { svg: () => tile('cb', C.sky, G.orbit, { sw: 2.3 }) },
  xr: { svg: () => tile('xr', C.night, `<g transform="rotate(45 12 12)">${G.rocket}</g>`, { sw: 1.9 }) },
  shield: { svg: () => tile('sh', C.mint, `<path d="${G.shield}"/><path d="M9.2 12.2l2 2 3.7-3.9"/>`, { sw: 2 }) },
  lock: { svg: () => tile('lk', C.slate, G.lock) },
  rates: { svg: () => tile('rt', C.blue, G.percent) },
  history: { svg: () => tile('hs', C.slate, G.history) },
  support: { svg: () => tile('sp', C.violet, G.chat) },
  // Быстро: молния дважды коротко «вздрагивает» масштабом.
  fast: {
    anim: true,
    svg: (t) => {
      const z = t == null ? 1 : 1 + 0.12 * bump(t, 0.08, 0.2) + 0.07 * bump(t, 0.22, 0.32);
      const scale = Math.abs(z - 1) > 0.0005 ? `translate(12 12) scale(${n(z)}) translate(-12 -12)` : '';
      return tile('ft', C.amber, G.bolt, { transform: scale });
    },
  },
  pause: { svg: () => tile('ps', C.slate, G.pause, { sw: 2.5 }) },
  ban: { svg: () => tile('bn', C.coral, G.ban) },
  // Тревога: от точки расходятся квадратные волны — критическое, нужен человек.
  alarm: {
    anim: true,
    svg: (t) => {
      let rings = '';
      let s = 1;
      if (t != null) {
        for (const off of [0, 0.5]) rings += ripple(C.coral, ((t - off + 1) % 1) / 0.6, 5.8, 5.4);
        s = 1 - 0.07 * Math.sin(2 * Math.PI * 2 * t);
      }
      const r = 6.2 * s;
      return `${rings}<rect x="${n(12 - r)}" y="${n(12 - r)}" width="${n(2 * r)}" height="${n(2 * r)}" rx="${n(r * 0.42)}" fill="${C.coral}"/>${glyph(
        '<path d="M12 9.2v3.2"/><circle cx="12" cy="15" r=".95" fill="#fff" stroke="none"/>',
        { sw: 2, scale: 1 },
      )}`;
    },
  },
  // На связи: мятная точка, от неё спокойная квадратная волна.
  live: {
    anim: true,
    svg: (t) => {
      const ring = t == null ? '' : ripple(C.mint, seg(t, 0, 0.8), 4.4, 6);
      return `${ring}<rect x="7.4" y="7.4" width="9.2" height="9.2" rx="3.9" fill="${C.mint}"/>`;
    },
  },
  robot: { svg: () => tile('rb', C.blue, G.robot, { sw: 2 }) },
  vault: { svg: () => tile('vt', C.mint, G.vault, { sw: 2 }) },
  flood: { svg: () => tile('fl', C.amber, G.hourglass, { sw: 2 }) },
  kill: { svg: () => tile('kl', C.coral, G.power, { sw: 2.3 }) },
  maint: { svg: () => tile('mt', C.slate, G.wrench, { sw: 2 }) },
  broadcast: { svg: () => tile('bc', C.sky, G.megaphone, { sw: 2 }) },
  limit: { svg: () => tile('lm', C.blue, G.gauge, { sw: 2 }) },
  // Праздник: первый обмен — плитка с галочкой «подпрыгивает», вокруг разлетаются искры.
  confetti: {
    anim: true,
    svg: (t) => {
      const pop = t == null ? 1 : 1 + 0.12 * bump(t, 0, 0.16) - 0.04 * bump(t, 0.16, 0.28);
      const cols = [C.amber, C.blue, C.coral, C.mint, C.violet, C.sky];
      let conf = '';
      for (let i = 0; i < 6; i++) {
        const ang = (i / 6) * 2 * Math.PI - Math.PI / 2 + 0.3;
        const p = t == null ? 0.6 : clamp(t / 0.55);
        if (t != null && (p <= 0.001 || p >= 0.999)) continue;
        const r = 5.6 + 5 * easeOut(p);
        // Частицы не тают, а уменьшаются: полупрозрачный цвет на тёмной теме грязнит.
        const k = t == null ? 1 : 1 - Math.pow(p, 1.8);
        if (k < 0.04) continue;
        const x = 12 + r * Math.cos(ang);
        const y = 12 + r * Math.sin(ang);
        conf += i % 2
          ? `<circle cx="${n(x)}" cy="${n(y)}" r="${n(1.1 * k)}" fill="${cols[i]}"/>`
          : `<path d="${SPARK(x, y, 1.8 * k)}" fill="${cols[i]}"/>`;
      }
      const s = n(0.64 * pop);
      return `${conf}<g transform="translate(12 12) scale(${s}) translate(-12 -12)">${tileBg('cf', C.mint)}${glyph(`<path d="${G.check}"/>`, { sw: 2.4 })}</g>`;
    },
  },
  // Шаги заявки.
  step_done: { svg: () => `<circle cx="12" cy="12" r="8.6" fill="${C.mint}"/>${glyph(`<path d="${G.check}"/>`, { sw: 2.5, scale: 0.78 })}` },
  step_now: {
    anim: true,
    svg: (t) => {
      const s = t == null ? 1 : 1 - 0.18 * bump(t, 0, 0.5);
      return `<circle cx="12" cy="12" r="7.9" fill="none" stroke="${C.blue}" stroke-width="2.3"/><circle cx="12" cy="12" r="${n(3.9 * s)}" fill="${C.blue}"/>`;
    },
  },
  step_todo: { svg: () => `<circle cx="12" cy="12" r="7.9" fill="none" stroke="${C.grey}" stroke-width="2.2"/>` },
  // Точки статусов.
  dot_ok: { svg: () => `<rect x="6.2" y="6.2" width="11.6" height="11.6" rx="4.9" fill="${C.mint}"/>` },
  dot_work: { svg: () => `<rect x="6.2" y="6.2" width="11.6" height="11.6" rx="4.9" fill="${C.blue}"/>` },
  dot_wait: {
    anim: true,
    svg: (t) => {
      const ring = t == null ? '' : ripple(C.amber, seg(t, 0, 0.8), 5.8, 4.6);
      return `${ring}<rect x="6.2" y="6.2" width="11.6" height="11.6" rx="4.9" fill="${C.amber}"/>`;
    },
  },
  dot_act: { svg: () => `<rect x="6.2" y="6.2" width="11.6" height="11.6" rx="4.9" fill="${C.coral}"/>` },
  dot_closed: { svg: () => `<rect x="6.2" y="6.2" width="11.6" height="11.6" rx="4.9" fill="${C.grey}"/>` },
  // Сегменты шкал: срок котировки, резервы, прогресс.
  seg_mint: { svg: () => SEG('sgm', C.mint) },
  seg_blue: { svg: () => SEG('sgb', C.blue) },
  seg_amber: { svg: () => SEG('sga', C.amber) },
  seg_coral: { svg: () => SEG('sgc', C.coral) },
  seg_empty: { svg: () => SEGR(C.grey) },
  // Текущий сегмент: по бледной полосе пробегает яркий блик.
  seg_now: {
    anim: true,
    svg: (t) => {
      // Сплошные цвета, без полупрозрачной заливки: одинаково чисто на тёмной и светлой теме.
      const base = SEGR(shade(C.blue, 0.42));
      if (t == null) return `${base}<rect x="8" y="${SEG_Y}" width="8" height="${SEG_H}" fill="${C.blue}"/>`;
      const k = ease(seg(t, 0, 0.7));
      if (k <= 0.001 || k >= 0.999) return base;
      const x = n(-10 + 34 * k);
      return `${base}<defs><clipPath id="snc"><rect x="${SEG_X}" y="${SEG_Y}" width="${SEG_W}" height="${SEG_H}" rx="${SEG_H / 2}"/></clipPath><linearGradient id="sng" x1="0" y1="0" x2="1" y2="0"><stop offset="0" stop-color="${C.blue}" stop-opacity="0"/><stop offset=".5" stop-color="${C.blue}" stop-opacity="1"/><stop offset="1" stop-color="${C.blue}" stop-opacity="0"/></linearGradient></defs><g clip-path="url(#snc)"><rect x="${x}" y="${SEG_Y - 1}" width="10" height="${SEG_H + 2}" fill="url(#sng)"/></g>`;
    },
  },
};

// ── Линейные значки (перекрашиваются Telegram) ───────────────────────────────────────
const M = (body) => body;
const MONO = {
  // Клиент
  swap: M(G.swap),
  ticket: M(G.ticket),
  invoice: M(`<path d="${G.receipt}"/>${G.receiptLines.map((d) => `<path d="${d}"/>`).join('')}`),
  rates: M(G.percent),
  history: M(G.history),
  support: M(G.chat),
  help: M('<circle cx="12" cy="12" r="8.6"/><path d="M9.7 9.5a2.4 2.4 0 0 1 4.6.9c0 1.6-2.3 2-2.3 3.4"/><circle cx="12" cy="16.6" r="1.05" fill="#fff" stroke="none"/>'),
  copy: M('<rect x="8.6" y="8.6" width="11" height="11" rx="2.4"/><path d="M15.4 8.6V6.8a2.4 2.4 0 0 0-2.4-2.4H6.8a2.4 2.4 0 0 0-2.4 2.4V13a2.4 2.4 0 0 0 2.4 2.4h1.8"/>'),
  claim: M('<path d="M12 4.4v10.2M7.8 10.6l4.2 4.2 4.2-4.2"/><path d="M5 15.8v1.6a2.4 2.4 0 0 0 2.4 2.4h9.2a2.4 2.4 0 0 0 2.4-2.4v-1.6"/>'),
  cb: M(G.orbit),
  xr: M(`<g transform="rotate(45 12 12)">${G.rocket}</g>`),
  usdt: M('<circle cx="12" cy="12" r="8.8"/><path d="M14.5 9.4c-.5-.9-1.4-1.4-2.5-1.4-1.5 0-2.5.7-2.5 1.8 0 2.6 5.2 1.5 5.2 4.2 0 1.2-1.1 2-2.7 2-1.3 0-2.2-.6-2.6-1.5M12 6.6v1.4M12 16v1.4"/>'),
  done: M(`<circle cx="12" cy="12" r="8.8"/><path d="M8.2 12.3l2.6 2.6 5-5.2"/>`),
  close: M('<path d="M7.4 7.4l9.2 9.2M16.6 7.4l-9.2 9.2"/>'),
  back: M('<path d="M19 12H5.6M10.6 7l-5 5 5 5"/>'),
  home: M('<path d="M4.6 11L12 4.8l7.4 6.2"/><path d="M6.6 9.6v8a1.6 1.6 0 0 0 1.6 1.6h7.6a1.6 1.6 0 0 0 1.6-1.6v-8"/><path d="M10.2 19.2v-4.4h3.6v4.4"/>'),
  refresh: M('<path d="M19 12a7 7 0 0 1-12.2 4.7M5 12a7 7 0 0 1 12.2-4.7"/><path d="M17.6 3.8v3.7h-3.7M6.4 20.2v-3.7h3.7"/>'),
  refund: M(`<path d="${G.uturn}"/><path d="${G.uturnHead}"/>`),
  wait: M(`${G.clockFace}<path d="M12 12V7.4M12 12l2.9 1.7"/>`),
  spin: M('<path d="M12 4.2a7.8 7.8 0 1 1-7.8 7.8"/>'),
  fast: M('<path d="M13.2 4L7 13.2h5l-1.2 6.8 6.2-9.2h-5z"/>'),
  gift: M('<rect x="4.8" y="9" width="14.4" height="3.6" rx="1.2"/><path d="M6.2 12.6v5.2a1.6 1.6 0 0 0 1.6 1.6h8.4a1.6 1.6 0 0 0 1.6-1.6v-5.2M12 9v10.4"/><path d="M12 9c-1.4-3.4-5-3.6-5-1.6 0 1.6 3.4 1.6 5 1.6zm0 0c1.4-3.4 5-3.6 5-1.6 0 1.6-3.4 1.6-5 1.6z"/>'),
  star: M('<path d="M12 4.4l2.3 4.7 5.1.7-3.7 3.6.9 5.1-4.6-2.4-4.6 2.4.9-5.1-3.7-3.6 5.1-.7z"/>'),
  globe: M('<circle cx="12" cy="12" r="8.4"/><path d="M3.8 12h16.4M12 3.6c2.2 2.3 3.3 5.1 3.3 8.4s-1.1 6.1-3.3 8.4c-2.2-2.3-3.3-5.1-3.3-8.4s1.1-6.1 3.3-8.4z"/>'),
  link: M('<path d="M10.4 13.6a3.6 3.6 0 0 0 5.1 0l2.6-2.6a3.6 3.6 0 0 0-5.1-5.1l-1 1"/><path d="M13.6 10.4a3.6 3.6 0 0 0-5.1 0L5.9 13a3.6 3.6 0 0 0 5.1 5.1l1-1"/>'),
  // Админка
  dash: M('<rect x="4.4" y="4.4" width="6.6" height="8" rx="1.8"/><rect x="13" y="4.4" width="6.6" height="5" rx="1.8"/><rect x="13" y="11.4" width="6.6" height="8.2" rx="1.8"/><rect x="4.4" y="14.4" width="6.6" height="5.2" rx="1.8"/>'),
  queue: M('<path d="M9.4 6.6h10M9.4 12h10M9.4 17.4h10"/><circle cx="5.4" cy="6.6" r="1.1" fill="#fff" stroke="none"/><circle cx="5.4" cy="12" r="1.1" fill="#fff" stroke="none"/><circle cx="5.4" cy="17.4" r="1.1" fill="#fff" stroke="none"/>'),
  order: M('<path d="M7.2 4.4h6.4l4.6 4.6v9a2.4 2.4 0 0 1-2.4 2.4H7.2A2.4 2.4 0 0 1 4.8 18V6.8a2.4 2.4 0 0 1 2.4-2.4z"/><path d="M13.4 4.6V9h4.4M8.4 13h7.2M8.4 16.4h4.6"/>'),
  user: M('<circle cx="12" cy="8.4" r="3.6"/><path d="M5.2 19.4a6.8 6.8 0 0 1 13.6 0"/>'),
  users: M('<circle cx="9.4" cy="8.6" r="3.2"/><path d="M3.8 19a5.6 5.6 0 0 1 11.2 0"/><path d="M15.4 5.6a3.2 3.2 0 0 1 0 6M17.8 13.8a5.6 5.6 0 0 1 2.6 5.2"/>'),
  ban: M(G.ban),
  search: M('<circle cx="10.8" cy="10.8" r="6.2"/><path d="M15.4 15.4l4.2 4.2"/>'),
  filter: M('<path d="M4.4 6h15.2l-5.8 6.8v5.4l-3.6 1.8v-7.2z"/>'),
  settings: M('<path d="M4.6 7h9.2M17.8 7h1.6M4.6 12h3.2M11.8 12h7.6M4.6 17h7.6M16.2 17h3.2"/><circle cx="15.8" cy="7" r="2"/><circle cx="9.8" cy="12" r="2"/><circle cx="14.2" cy="17" r="2"/>'),
  bell: M('<path d="M6.6 10.2a5.4 5.4 0 0 1 10.8 0c0 4.9 2 6.6 2 6.6H4.6s2-1.7 2-6.6"/><path d="M10.4 19.6a1.9 1.9 0 0 0 3.2 0"/>'),
  vault: M(G.vault),
  robot: M(G.robot),
  megaphone: M(G.megaphone),
  journal: M('<path d="M5.6 5.4a1.6 1.6 0 0 1 1.6-1.6h10.6v15.4H7.2a1.6 1.6 0 0 0-1.6 1.6z"/><path d="M5.6 18.8V20M9.4 8.2h5M9.4 11.4h5"/>'),
  chart: M('<path d="M4.4 4.6v13a2 2 0 0 0 2 2h13.2"/><path d="M8.6 15.4v-3.2M12.6 15.4V8.4M16.6 15.4v-5"/>'),
  // Выгрузка: стрелка выходит из ящика вверх-вправо (не путать с «Забрать» — стрелка вниз).
  export: M('<path d="M13.4 4.6h6v6M19.4 4.6l-8 8"/><path d="M17.4 14v3.2a2.4 2.4 0 0 1-2.4 2.4H7a2.4 2.4 0 0 1-2.4-2.4V9.2A2.4 2.4 0 0 1 7 6.8h3.2"/>'),
  emoji: M('<circle cx="12" cy="12" r="8.4"/><path d="M8.6 13.8a4.2 4.2 0 0 0 6.8 0"/><circle cx="9.3" cy="9.8" r="1.05" fill="#fff" stroke="none"/><circle cx="14.7" cy="9.8" r="1.05" fill="#fff" stroke="none"/>'),
  staff: M('<rect x="5" y="4.6" width="14" height="15" rx="2.6"/><circle cx="12" cy="10.2" r="2.6"/><path d="M8.4 16.6a3.8 3.8 0 0 1 7.2 0M10 4.6v1.6h4V4.6"/>'),
  key: M('<circle cx="8.4" cy="15.6" r="3.6"/><path d="M11 13l7.6-7.6M16.4 7.6l2.2 2.2M14.2 9.8l1.8 1.8"/>'),
  power: M(G.power),
  pause: M(G.pause),
  play: M('<path d="M8.6 6.4v11.2l9-5.6z"/>'),
  maint: M(G.wrench),
  shield: M(`<path d="${G.shield}"/><path d="M9.2 12.2l2 2 3.7-3.9"/>`),
  lock: M(G.lock),
  eye: M('<path d="M3.6 12s3-5.8 8.4-5.8 8.4 5.8 8.4 5.8-3 5.8-8.4 5.8S3.6 12 3.6 12z"/><circle cx="12" cy="12" r="2.6"/>'),
  edit: M('<path d="M15.4 5.2a2.1 2.1 0 0 1 3 3L9 17.6l-4 1 1-4z"/><path d="M13.8 6.8l3 3"/>'),
  plus: M('<path d="M12 5.6v12.8M5.6 12h12.8"/>'),
  minus: M('<path d="M5.6 12h12.8"/>'),
  trash: M('<path d="M4.8 7h14.4M9.6 7V5.4a1 1 0 0 1 1-1h2.8a1 1 0 0 1 1 1V7M6.8 7l.8 11.2a1.8 1.8 0 0 0 1.8 1.6h5.2a1.8 1.8 0 0 0 1.8-1.6L17.2 7"/>'),
  calendar: M('<rect x="4.6" y="5.6" width="14.8" height="14" rx="2.6"/><path d="M4.6 10h14.8M8.6 3.8v3.4M15.4 3.8v3.4"/>'),
  limit: M(G.gauge),
  alert: M('<path d="M10.5 5.2a1.7 1.7 0 0 1 3 0l6.2 11a1.7 1.7 0 0 1-1.5 2.6H5.8a1.7 1.7 0 0 1-1.5-2.6z"/><path d="M12 9.6v3.4"/><circle cx="12" cy="15.8" r="1" fill="#fff" stroke="none"/>'),
  flag: M('<path d="M6.2 20V4.8"/><path d="M6.2 5.2c4-2 7.4 2 11.6 0v8.4c-4.2 2-7.6-2-11.6 0"/>'),
  pin: M('<path d="M12 15.6v4.6"/><path d="M8.6 4.6h6.8M9.6 4.6v4.6l-2.6 3v1.4h10v-1.4l-2.6-3V4.6"/>'),
  topic: M('<path d="M5 6.8a2 2 0 0 1 2-2h10a2 2 0 0 1 2 2v7.4a2 2 0 0 1-2 2h-6.4L7 19.2v-3H7a2 2 0 0 1-2-2z"/><path d="M8.6 9h6.8M8.6 12h4.4"/>'),
  ack: M('<path d="M4.8 12.6l3.6 3.6 7.6-8"/><path d="M12.8 15.4l.8.8 7.6-8"/>'),
  take: M('<path d="M8.6 11V6.4a1.6 1.6 0 0 1 3.2 0V11M11.8 10.2V5.2a1.6 1.6 0 0 1 3.2 0v5M15 10.2V6.8a1.6 1.6 0 0 1 3.2 0v6.6a6.2 6.2 0 0 1-6.2 6.2h-.6a6.2 6.2 0 0 1-5.2-2.8l-2-3.2a1.6 1.6 0 0 1 2.6-1.8l1 1.2"/>'),
  coin: M('<circle cx="9.6" cy="9.6" r="5.4"/><path d="M15.6 9.4a5.4 5.4 0 1 1-6.2 6.2"/>'),
  sparkle: M(`<path d="${SPARK(10.6, 12.6, 7.6)}"/><path d="M18.4 3.8v3.6M16.6 5.6h3.6"/>`),
};

// Подписи — для превью-листа и админки «Эмодзи».
const LABELS = {
  done: 'Готово', swap: 'Обмен', process: 'В работе', wait: 'Ждём', err: 'Ошибка', warn: 'Внимание',
  refund: 'Возврат', manual: 'Ручная проверка', invoice: 'Счёт', sending: 'Чек в пути', usdt: 'USDT',
  cb: 'CryptoBot', xr: 'xRocket', shield: 'Защита', lock: 'Приватность', rates: 'Курс', history: 'История',
  support: 'Поддержка', fast: 'Быстро', pause: 'Пауза', ban: 'Бан', alarm: 'Тревога', live: 'На связи',
  robot: 'Юзербот', vault: 'Резерв', flood: 'Лимит Telegram', kill: 'Стоп-кран', maint: 'Обслуживание',
  broadcast: 'Рассылка', limit: 'Лимиты', confetti: 'Первый обмен', step_done: 'Шаг: готов', step_now: 'Шаг: сейчас',
  step_todo: 'Шаг: впереди', dot_ok: 'Готово', dot_work: 'В работе', dot_wait: 'Ждём', dot_act: 'Ход за вами',
  dot_closed: 'Закрыта', seg_mint: 'Шкала: есть', seg_blue: 'Шкала: прошло', seg_amber: 'Шкала: мало',
  seg_coral: 'Шкала: на исходе', seg_empty: 'Шкала: пусто', seg_now: 'Шкала: сейчас',
  ticket: 'Чек', help: 'Помощь', copy: 'Копировать', claim: 'Забрать', close: 'Закрыть', back: 'Назад', home: 'Главная',
  refresh: 'Обновить', spin: 'Загрузка', gift: 'Подарок', star: 'Звезда', globe: 'Язык', link: 'Ссылка', dash: 'Сводка',
  queue: 'Очередь', order: 'Заявка', user: 'Клиент', users: 'Клиенты', search: 'Поиск', filter: 'Фильтр',
  settings: 'Настройки', bell: 'Алерты', megaphone: 'Рассылка', journal: 'Журнал', chart: 'Статистика',
  export: 'Экспорт', emoji: 'Эмодзи', staff: 'Персонал', key: 'Ключи', power: 'Стоп-кран', play: 'Пуск',
  eye: 'Смотреть', edit: 'Изменить', plus: 'Добавить', minus: 'Убрать', trash: 'Удалить', calendar: 'Период',
  alert: 'Внимание', flag: 'Отметить', pin: 'Закрепить', topic: 'Тема', ack: 'Принял', take: 'Взять',
  coin: 'Деньги', sparkle: 'Новое',
};

module.exports = { C, COLOR, MONO, LABELS };
