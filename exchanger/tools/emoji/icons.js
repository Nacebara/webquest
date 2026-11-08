// Премиум-эмодзи exch, версия 2: одна семья значков на сетке 24×24, из неё tgs.js собирает два
// набора кастомных эмодзи Telegram (TGS — вектор Lottie, 512×512, 60 кадров/с, петля 2,8 с).
//
// • COLOR — плитки со скруглением, мягким диагональным градиентом и стеклянным бликом, белый
//   знак; точки статусов, шаги, кольца прогресса, монета USDT, знаки кошельков. Telegram их не
//   перекрашивает: цвет несёт смысл.
// • MONO — тонкая линия (MONO_W), белая; набор ставится с needs_repainting: Telegram красит его
//   в цвет текста или кнопки. На кнопках бот всегда ставит моно.
//
// Анимированы все значки. Значок — функция (t) => SVG: t — время кадра 0..1, null — статичный
// кадр (превью, клиенты без анимации). Правило движения одно на всю семью: короткий жест в начале
// петли (0,1–0,5 с), дальше покой до конца петли. Так даже десяток значков на экране не мельтешит.
// Постоянно движется только то, что идёт прямо сейчас: «в работе», «ждём», «в пути», тревога.
// Кадры t = 0 и t = 1 совпадают (check.js сверяет по пикселям).
//
// Чистота на обеих темах: только сплошные цвета. Исчезающее сжимается или истончается, а не тает:
// полупрозрачный цвет на тёмном фоне выглядит грязным пятном.

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
  // Стрелка «Вниз» владельца — его цвет.
  arrow: '#3F82F0',
  // Пламя для текста (вариант А владельца).
  flame: '#E8890C',
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

// ── Преобразования и жесты ─────────────────────────────────────────────────────────────
// Пустая строка — «без преобразования»: так конвертер видит покой и не пишет лишних ключей.
const at = (cx, cy, inner) => `translate(${cx} ${cy}) ${inner} translate(${-cx} ${-cy})`;
const scaleAt = (z, cx = 12, cy = 12) => (Math.abs(z - 1) < 0.0005 ? '' : at(cx, cy, `scale(${n(z)})`));
const scaleXY = (sx, sy, cx = 12, cy = 12) =>
  Math.abs(sx - 1) < 0.0005 && Math.abs(sy - 1) < 0.0005 ? '' : at(cx, cy, `scale(${n(sx)} ${n(sy)})`);
const rotAt = (a, cx = 12, cy = 12) => (Math.abs(a) < 0.01 ? '' : `rotate(${n(a)} ${cx} ${cy})`);
const move = (dx, dy) => (Math.abs(dx) < 0.001 && Math.abs(dy) < 0.001 ? '' : `translate(${n(dx)} ${n(dy)})`);
const grp = (tr, body) => (tr ? `<g transform="${tr}">${body}</g>` : body);

const A = {
  // Мягкий «пульс» масштабом: вырос, чуть просел, вернулся.
  pop: (t, a = 0.1, cx = 12, cy = 12) =>
    t == null ? '' : scaleAt(1 + a * bump(t, 0.06, 0.26) - a * 0.3 * bump(t, 0.26, 0.4), cx, cy),
  // Поворот на deg с пружинкой. Рисунок должен быть симметричен повороту на deg: в конце жеста
  // преобразование снимается, и кадр совпадает с первым.
  turn: (t, deg, cx = 12, cy = 12, a = 0.06, b = 0.5) => {
    if (t == null) return '';
    const ang = deg * easeBack(seg(t, a, b));
    return Math.abs(ang - deg) < 0.01 ? '' : rotAt(ang, cx, cy);
  },
  // Плавный оборот без перелёта (стрелки обновления, циферблат).
  spin: (t, deg, cx = 12, cy = 12, a = 0.06, b = 0.6) => {
    if (t == null) return '';
    const ang = deg * ease(seg(t, a, b));
    return Math.abs(ang - deg) < 0.01 ? '' : rotAt(ang, cx, cy);
  },
  // Качание вокруг оси с затуханием (колокольчик, ключ, гаечный ключ).
  swing: (t, deg, cx = 12, cy = 12, cycles = 2, a = 0.06, b = 0.56) => {
    if (t == null) return '';
    const k = seg(t, a, b);
    return rotAt(deg * Math.sin(2 * Math.PI * cycles * k) * Math.pow(1 - k, 1.3), cx, cy);
  },
  // Шаг в сторону и обратно.
  nudge: (t, dx, dy, a = 0.06, b = 0.34) => (t == null ? '' : move(dx * bump(t, a, b), dy * bump(t, a, b))),
  // Дрожь по горизонтали с затуханием.
  shake: (t, amp = 1, cycles = 3, a = 0.06, b = 0.4) => {
    if (t == null) return '';
    const k = seg(t, a, b);
    return move(amp * Math.sin(2 * Math.PI * cycles * k) * (1 - k), 0);
  },
  // Моргание: сжатие по вертикали. Только для залитых фигур без обводки (глаза-точки): обводку
  // неравномерный масштаб сплющил бы. Линии «моргают» геометрией — см. eye.
  blink: (t, cy = 12, a = 0.18) => (t == null ? '' : scaleXY(1, 1 - 0.88 * bump(t, a, a + 0.1), 12, cy)),
};
// Сколько «прижать» на жесте: 0 в покое, до 1 на пике — для жестов геометрией.
const press = (t, a, b) => (t == null ? 0 : bump(t, a, b));

// ── Плитка ─────────────────────────────────────────────────────────────────────────────
const TILE = { x: 1.4, y: 1.4, s: 21.2, r: 6.6 };
const GLYPH_SCALE = 0.84;
const GLYPH_W = 1.8; // после масштаба 0,84 → 1,5: тоньше первой версии (2,1), на 20 px ещё не мерцает
// Линия моно-значков (во viewBox 25,2: на 20 px ≈ 1,3 px).
const MONO_W = 1.6;

const tileRect = (extra = '') =>
  `<rect x="${TILE.x}" y="${TILE.y}" width="${TILE.s}" height="${TILE.s}" rx="${TILE.r}"${extra}/>`;

// Плитка с мягким диагональным градиентом (сверху слева светлее, снизу справа глубже) и стеклом.
// Перепад +0,12 / −0,08: спокойнее первой версии и без ступенек — rlottie считает градиент в
// 8 битах, на более крутом перепаде коралловая плитка расходится с эталоном (verify_tgs.py).
const tileBg = (id, color) =>
  `<defs><linearGradient id="${id}g" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="${shade(color, 0.12)}"/><stop offset="1" stop-color="${shade(color, -0.08)}"/></linearGradient>` +
  `<linearGradient id="${id}h" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#fff" stop-opacity=".16"/><stop offset=".7" stop-color="#fff" stop-opacity="0"/></linearGradient></defs>` +
  tileRect(` fill="url(#${id}g)"`) +
  tileRect(` fill="url(#${id}h)"`);

// Белый знак; масштаб — от центра значка, после собственного преобразования знака.
const glyph = (body, opts = {}) => {
  const tr = [opts.transform, `translate(12 12) scale(${opts.scale || GLYPH_SCALE}) translate(-12 -12)`]
    .filter(Boolean)
    .join(' ');
  return `<g fill="none" stroke="${W}" stroke-width="${opts.sw || GLYPH_W}" stroke-linecap="round" stroke-linejoin="round" transform="${tr}">${body}</g>`;
};

const tile = (id, color, body, opts = {}) => `${tileBg(id, color)}${opts.under || ''}${glyph(body, opts)}${opts.over || ''}`;

// Стрелка «Вниз» по картинке владельца (8 ноября 2026): там линия 77 px, ствол 636 px от
// верхнего конца до острия, «крылья» по 268 px под 45°, концы и угол круглые. Здесь — тот же
// рисунок высотой 15,6 из 24 (как наши моно-значки), по центру.
const DOWN = { w: 1.69, d: 'M12 5.04V18.96M6.14 13.09L12 18.96L17.86 13.09' };

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
  // Пламя: внешний язык и внутренний огонёк (огонёк «Выгодно», DESIGN-v0.2 Р15).
  flame:
    'M12 20.4C8.6 20.4 6.4 18 6.4 15.1C6.4 12.2 8.4 10.6 9.8 8.6C10.8 7.2 11.4 5.6 11.2 3.8C14.4 5.4 15.6 8 15.4 10.6C16.2 10 16.8 9.2 17 8.2C17.8 9.6 17.6 12.4 17.6 15.1C17.6 18 15.4 20.4 12 20.4Z',
  flameCore: 'M12 20.4C10.4 20.4 9.4 19.2 9.4 17.8C9.4 16.2 10.6 15.2 11.6 13.8C12.4 15 14.6 16 14.6 17.8C14.6 19.2 13.6 20.4 12 20.4Z',
  // Весы: стойка и основание неподвижны, коромысло с чашами качается.
  scalePost: '<path d="M12 5.2v13.8M8 19h8"/>',
  scaleBeam:
    '<path d="M4.6 7.6h14.8"/><path d="M4.6 7.6l-2.3 5.6M4.6 7.6l2.3 5.6M19.4 7.6l-2.3 5.6M19.4 7.6l2.3 5.6"/><path d="M2.3 13.2a2.3 2.3 0 0 0 4.6 0zM17.1 13.2a2.3 2.3 0 0 0 4.6 0z"/>',
  // Ступени: лесенка вверх со стрелкой — комиссия по сумме.
  stairs: '<path d="M3.8 19.6h4.4v-4.4h4.4v-4.4h4.4V6.4h3.4"/>',
  stairsArrow: '<path d="M14.6 4.2h5.6v5.6"/>',
  check: 'M7 12.4l3.3 3.3L17 9',
  cross1: 'M8 8l8 8',
  cross2: 'M16 8l-8 8',
  exclBar: 'M12 6.6v6.6',
  exclDot: `<circle cx="12" cy="17" r="1.3" fill="${W}" stroke="none"/>`,
  // Обмен: две встречные стрелки, центрально-симметричны — поворот на 180° даёт тот же рисунок.
  swap: '<path d="M5.6 9h12.2M14.8 5.9l3.1 3.1-3.1 3.1"/><path d="M18.4 15H6.2M9.2 11.9L6.1 15l3.1 3.1"/>',
  // Чек: билет с вырезами по бокам и перфорацией.
  ticket:
    '<path d="M4.6 8.6a1.6 1.6 0 0 1 1.6-1.6h11.6a1.6 1.6 0 0 1 1.6 1.6v1.5a1.9 1.9 0 0 0 0 3.8v1.5a1.6 1.6 0 0 1-1.6 1.6H6.2a1.6 1.6 0 0 1-1.6-1.6v-1.5a1.9 1.9 0 0 0 0-3.8z"/><path d="M14.4 8.9v6.2" stroke-dasharray="1.3 1.75"/>',
  receipt: 'M7.2 5h9.6v14l-1.6-1.1-1.6 1.1-1.6-1.1-1.6 1.1-1.6-1.1-1.6 1.1z',
  receiptLines: ['M9.8 8.8h4.4', 'M9.8 11.9h4.4', 'M9.8 15h2.7'],
  // Возврат: путь от хвоста к острию.
  uturn: 'M11.2 18.4h3.2a4.2 4.2 0 0 0 0-8.4H6.2',
  uturnHead: 'M9.6 6.6L6.2 10l3.4 3.4',
  clockFace: '<circle cx="12" cy="12" r="7.1"/>',
  shield: 'M12 3.9l6.6 2.4v5c0 4.2-2.8 7.1-6.6 8.7-3.8-1.6-6.6-4.5-6.6-8.7v-5z',
  shieldCheck: 'M9.2 12.2l2 2 3.7-3.9',
  lockBody: '<rect x="6.6" y="10.8" width="10.8" height="8.2" rx="2.2"/>',
  lockShackle: '<path d="M8.9 10.8V8.6a3.1 3.1 0 0 1 6.2 0v2.2"/>',
  // Процент центрально-симметричен: поворот на 180° даёт тот же рисунок.
  percent: `<path d="M16.6 7.4l-9.2 9.2"/><circle cx="8.4" cy="8.4" r="1.75"/><circle cx="15.6" cy="15.6" r="1.75"/>`,
  historyArc: '<path d="M5.6 12a6.4 6.4 0 1 0 1.95-4.6"/><path d="M5.3 4.6v3.3h3.3"/>',
  historyHands: '<path d="M12 8.8v3.6l2.4 1.5"/>',
  chat: '<path d="M5.6 8.2a2.4 2.4 0 0 1 2.4-2.4h8a2.4 2.4 0 0 1 2.4 2.4v5.4a2.4 2.4 0 0 1-2.4 2.4h-4.4l-3.7 2.9V16a2.3 2.3 0 0 1-2.3-2.4z"/>',
  bolt: `<path d="M13.2 4.4L7.4 13.1h4.8l-1.3 6.5 5.7-8.6h-4.7z" fill="${W}" stroke-width="1"/>`,
  power: '<path d="M12 4.8v6.4"/><path d="M8 7.4a6.2 6.2 0 1 0 8 0"/>',
  robotBody: `<rect x="6.2" y="8.6" width="11.6" height="9" rx="2.8"/><path d="M12 8.6V6"/><circle cx="12" cy="5.4" r=".85" fill="${W}" stroke="none"/>`,
  robotEyes: `<circle cx="9.7" cy="13" r="1.15" fill="${W}" stroke="none"/><circle cx="14.3" cy="13" r="1.15" fill="${W}" stroke="none"/>`,
  vaultBox: '<rect x="5.4" y="5.8" width="13.2" height="12.4" rx="2.4"/>',
  // Ручка сейфа: четыре спицы — поворот на 90° даёт тот же рисунок.
  vaultDial: '<circle cx="12" cy="12" r="2.8"/><path d="M12 7.6v1.6M12 14.8v1.6M7.6 12h1.6M14.8 12h1.6"/>',
  hourglass: '<path d="M8.4 5.6h7.2M8.4 18.4h7.2M9.4 5.7v1.6c0 1.7 2.6 2.9 2.6 4.7s-2.6 3-2.6 4.7v1.6M14.6 5.7v1.6c0 1.7-2.6 2.9-2.6 4.7s2.6 3 2.6 4.7v1.6"/>',
  // Знак CryptoBot в exch — «орбита»: дуга с точкой в просвете (не логотип кошелька).
  orbit: `<path d="M16.7 8.3a6.1 6.1 0 1 0 0 7.4"/><circle cx="17.3" cy="12" r="1.45" fill="${W}" stroke="none"/>`,
  // Знак xRocket в exch — ракета (не логотип кошелька), строится вверх и поворачивается на 45°.
  rocket:
    '<path d="M12 4.4c2.5 1.9 3.7 4.6 3.7 7.6v3H8.3v-3c0-3 1.2-5.7 3.7-7.6z"/><circle cx="12" cy="10.6" r="1.35"/><path d="M8.3 12.9l-1.9 1.8V17h1.9M15.7 12.9l1.9 1.8V17h-1.9M12 17.4v1.8"/>',
  magnifier: '<circle cx="10.6" cy="10.6" r="3.9"/><path d="M13.5 13.5l3.2 3.2"/>',
  docLines: '<path d="M6 7.6h12M6 12h12M6 16.4h7.4"/>',
  wrench: '<path d="M14.6 6.1a3.9 3.9 0 0 0-4.7 5l-4.2 4.2a1.6 1.6 0 0 0 2.3 2.3l4.2-4.2a3.9 3.9 0 0 0 5-4.7l-2.3 2.3-2.4-.6-.6-2.4z"/>',
  banRing: '<circle cx="12" cy="12" r="6.6"/>',
  banSlash: '<path d="M7.4 7.4l9.2 9.2"/>',
  megaphone: '<path d="M6 10.4v3.2a1 1 0 0 0 1 1h2.2l5.6 3.4V6l-5.6 3.4H7a1 1 0 0 0-1 1z"/>',
  megaWave: '<path d="M17.6 9.6a3.4 3.4 0 0 1 0 4.8"/>',
  gaugeArc: `<path d="M5.4 15.6a6.8 6.8 0 1 1 13.2 0"/><circle cx="12" cy="13.6" r=".85" fill="${W}" stroke="none"/>`,
  gaugeNeedle: '<path d="M12 13.4l3-3.6"/>',
};

const L = { receiptLine: 4.4 };

// Квадратная «волна»: скруглённый квадрат выходит из-под фигуры, расходится и истончается.
const ripple = (color, k, base = 5, grow = 5.6) => {
  if (k <= 0.001 || k >= 0.999) return '';
  const s = base + grow * easeOut(k);
  const w = 1.7 * Math.pow(1 - k, 1.25);
  if (w < 0.06) return '';
  return `<rect x="${n(12 - s)}" y="${n(12 - s)}" width="${n(2 * s)}" height="${n(2 * s)}" rx="${n(s * 0.42)}" fill="none" stroke="${color}" stroke-width="${n(w)}"/>`;
};

const coin = (id, sheenK = 0, tr = '') => {
  const sh =
    sheenK > 0 && sheenK < 1
      ? `<defs><clipPath id="${id}c"><circle cx="12" cy="12" r="10.4"/></clipPath><linearGradient id="${id}s" x1="0" y1="0" x2="1" y2="0"><stop offset="0" stop-color="#fff" stop-opacity="0"/><stop offset=".5" stop-color="#fff" stop-opacity=".4"/><stop offset="1" stop-color="#fff" stop-opacity="0"/></linearGradient></defs><g clip-path="url(#${id}c)"><rect x="${n(-10 + 34 * sheenK)}" y="-8" width="5.5" height="40" fill="url(#${id}s)" transform="rotate(20 12 12)"/></g>`
      : '';
  return (
    `<defs><linearGradient id="${id}g" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="${shade(C.coin, 0.16)}"/><stop offset="1" stop-color="${shade(C.coin, -0.08)}"/></linearGradient></defs>` +
    `<circle cx="12" cy="12" r="10.6" fill="${C.coinRim}"/><circle cx="12" cy="12" r="9.1" fill="url(#${id}g)"/>` +
    `<circle cx="12" cy="12" r="7.6" fill="none" stroke="${W}" stroke-opacity=".3" stroke-width=".9"/>` +
    sh +
    glyph(
      '<path d="M14.7 9.2c-.5-1-1.5-1.6-2.7-1.6-1.6 0-2.7.8-2.7 2 0 2.8 5.6 1.6 5.6 4.5 0 1.3-1.2 2.2-2.9 2.2-1.4 0-2.4-.6-2.8-1.6M12 6v1.6M12 16.3v1.7"/>',
      { scale: 0.86, sw: 1.9, transform: tr },
    )
  );
};

// Пауза: полоски по очереди коротко приседают (геометрией — толщина линии не меняется).
const PAUSE = (t) =>
  [
    [9.6, 0.06, 0.24],
    [14.4, 0.16, 0.34],
  ]
    .map(([x, a, b]) => `<path d="M${x} ${n(7.6 + 2.6 * press(t, a, b))}V16.4"/>`)
    .join('');

// Точка статуса: скруглённый квадрат; жест — короткий пульс.
const DOT = (color, t, amp = 0.12, twice = false) => {
  let z = 1;
  if (t != null) z = 1 - amp * bump(t, 0.06, 0.3) - (twice ? amp * bump(t, 0.32, 0.56) : 0);
  const r = 5.8 * z;
  return `<rect x="${n(12 - r)}" y="${n(12 - r)}" width="${n(2 * r)}" height="${n(2 * r)}" rx="${n(r * 0.42)}" fill="${color}"/>`;
};

// Кольцо прогресса: серая дорожка и синяя дуга на долю frac; дуга делает один плавный оборот —
// «идёт», длина дуги — сколько готово.
const PROG = (frac) => (t) => {
  const r = 7.6;
  const len = 2 * Math.PI * r;
  const a = t == null ? 0 : 360 * ease(seg(t, 0.06, 0.7));
  const rot = Math.abs(a - 360) < 0.01 ? 0 : a;
  return (
    `<circle cx="12" cy="12" r="${r}" fill="none" stroke="${shade(C.grey, 0.38)}" stroke-width="2.6"/>` +
    `<g transform="rotate(${n(rot - 90)} 12 12)"><circle cx="12" cy="12" r="${r}" fill="none" stroke="${C.blue}" stroke-width="2.6" stroke-linecap="round" stroke-dasharray="${n(len * frac)} ${n(len * 1.5)}"/></g>`
  );
};

// ── Цветные значки ──────────────────────────────────────────────────────────────────
const COLOR = {
  // Готово: галочка мягко «подпрыгивает», в углу вспыхивает искра. Один слой без матта.
  done: (t) => {
    const tw = t == null ? 0 : bump(t, 0.22, 0.56);
    const spark = tw > 0.02 ? `<path d="${SPARK(18, 6.1, 2.6 * tw)}" fill="${W}"/>` : '';
    return tile('dn', C.mint, `<path d="${G.check}"/>`, { transform: A.pop(t, 0.11), over: spark, sw: 2.1 });
  },
  // Обмен — фирменный: стрелки делают пол-оборота с пружинкой и стоят.
  swap: (t) => tile('sw', C.blue, G.swap, { transform: A.turn(t, 180, 12, 12, 0.08, 0.5) }),
  // В работе: три точки бегут волной — всю петлю, ровно два прохода.
  process: (t) => {
    const dots = [8, 12, 16]
      .map((x, i) => {
        const w = t == null ? 0 : Math.pow(0.5 - 0.5 * Math.cos(2 * Math.PI * (2 * t - i * 0.14)), 3);
        return `<circle cx="${x}" cy="${n(12 - 2.4 * w)}" r="1.55" fill="${W}" stroke="none"/>`;
      })
      .join('');
    return tile('pr', C.blue, dots);
  },
  // Ждём: минутная стрелка делает оборот за петлю.
  wait: (t) => {
    const a = t == null ? 0 : 360 * t;
    const rot = Math.abs(a % 360) < 0.01 ? '' : rotAt(a % 360);
    return tile('wt', C.amber, `${G.clockFace}<path d="M12 12l2.9 1.7"/>${grp(rot, '<path d="M12 12V7.4"/>')}`);
  },
  // Ошибка: крест коротко вздрагивает; плитка стоит.
  err: (t) => tile('er', C.coral, `<path d="${G.cross1}"/><path d="${G.cross2}"/>`, { transform: A.shake(t, 1.1), sw: 2.1 }),
  // Внимание: восклицательный знак коротко подпрыгивает.
  warn: (t) => tile('wn', C.amber, `<path d="${G.exclBar}"/>${G.exclDot}`, { transform: A.nudge(t, 0, -1.4, 0.06, 0.3), sw: 2.1 }),
  // Вниз — стрелка владельца (8 ноября 2026), точь-в-точь по его картинке: без плитки, голубая
  // линия со скруглёнными концами, «крылья» под 45°. Дважды мягко «кивает» вниз — на кнопку
  // под сообщением.
  down: (t) => {
    const dy = t == null ? 0 : 1.7 * bump(t, 0.06, 0.28) + 0.9 * bump(t, 0.28, 0.46);
    return `<g fill="none" stroke="${C.arrow}" stroke-width="${DOWN.w}" stroke-linecap="round" stroke-linejoin="round">${grp(move(0, dy), `<path d="${DOWN.d}"/>`)}</g>`;
  },
  // Возврат: стрелка дважды мягко «тянется» назад.
  refund: (t) => {
    const dx = t == null ? 0 : -1.5 * bump(t, 0.06, 0.28) - 0.8 * bump(t, 0.28, 0.44);
    return tile('rf', C.violet, `<path d="${G.uturn}"/><path d="${G.uturnHead}"/>`, { transform: move(dx, 0) });
  },
  // Ручная проверка: лупа неторопливо скользит по строкам всю петлю.
  manual: (t) => {
    const dx = t == null ? 0 : 2.6 * Math.sin(2 * Math.PI * t);
    const dy = t == null ? 0 : 1.2 * Math.sin(4 * Math.PI * t);
    return tile('mn', C.violet, `<g stroke="${shade(C.violet, 0.5)}">${G.docLines}</g>${grp(move(dx, dy), G.magnifier)}`);
  },
  // Счёт: строки «печатаются» по одной и стоят; в конце петли сжимаются к началу строки.
  invoice: (t) => {
    const lines = G.receiptLines
      .map((d, i) => {
        if (t == null) return `<path d="${d}"/>`;
        const len = i === 2 ? 2.7 : L.receiptLine;
        const k = ease(seg(t, 0.08 + i * 0.12, 0.2 + i * 0.12)) * (1 - ease(seg(t, 0.86, 0.98)));
        return drawOn(d, len, k);
      })
      .join('');
    return tile('iv', C.sky, `<path d="${G.receipt}"/>${lines}`);
  },
  // Чек в пути: билет плывёт, полосы скорости уплывают назад и истончаются.
  sending: (t) => {
    const lines = [
      [8.6, 3.2],
      [12, 4.4],
      [15.4, 3],
    ]
      .map(([y, len], i) => {
        if (t == null) return `<path d="M${n(7.2 - len)} ${y}h${len}" stroke="${C.mint}" stroke-width="1.5" stroke-linecap="round"/>`;
        const ph = (t * 3 + i * 0.33) % 1;
        const x = 8.2 - len - 2.4 * ph;
        const w = 1.6 * Math.sin(Math.PI * ph);
        if (w < 0.08) return '';
        return `<path d="M${n(x)} ${y}h${len}" stroke="${C.mint}" stroke-width="${n(w)}" stroke-linecap="round"/>`;
      })
      .join('');
    const dy = t == null ? 0 : 0.55 * Math.sin(2 * Math.PI * 3 * t);
    const body =
      `<defs><linearGradient id="sdg" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="${shade(C.mint, 0.14)}"/><stop offset="1" stop-color="${shade(C.mint, -0.1)}"/></linearGradient></defs>` +
      '<path d="M8.4 8.2a1.4 1.4 0 0 1 1.4-1.4h10.4a1.4 1.4 0 0 1 1.4 1.4v1.5a2.3 2.3 0 0 0 0 4.6v1.5a1.4 1.4 0 0 1-1.4 1.4H9.8a1.4 1.4 0 0 1-1.4-1.4v-1.5a2.3 2.3 0 0 0 0-4.6z" fill="url(#sdg)"/>' +
      `<path d="M17.2 8.9v6.2" fill="none" stroke="${W}" stroke-width="1.4" stroke-linecap="round" stroke-dasharray="1.3 1.75"/>`;
    return `${lines}${grp(move(0, dy), body)}`;
  },
  // Монета USDT: своя (не логотип); раз за петлю по ней проходит блик.
  usdt: (t) => coin('cn', t == null ? 0 : ease(seg(t, 0.1, 0.55))),
  // Знаки кошельков в exch — свои рисунки на фирменных плитках.
  cb: (t) => tile('cb', C.sky, G.orbit, { transform: A.spin(t, 360, 12, 12, 0.06, 0.62), sw: 2 }),
  xr: (t) =>
    tile('xr', C.night, `<g transform="rotate(45 12 12)">${G.rocket}</g>`, { transform: A.nudge(t, 1.1, -1.1, 0.06, 0.36), sw: 1.65 }),
  shield: (t) => tile('sh', C.mint, `<path d="${G.shield}"/><path d="${G.shieldCheck}"/>`, { transform: A.pop(t, 0.08), sw: 1.75 }),
  lock: (t) => tile('lk', C.slate, `${G.lockBody}${grp(A.nudge(t, 0, -1.3, 0.06, 0.32), G.lockShackle)}`),
  rates: (t) => tile('rt', C.blue, G.percent, { transform: A.turn(t, 180) }),
  history: (t) => tile('hs', C.slate, `${G.historyArc}${grp(A.spin(t, -360, 12, 12, 0.06, 0.6), G.historyHands)}`),
  support: (t) => tile('sp', C.violet, G.chat, { transform: A.pop(t, 0.09, 12, 18) }),
  // Быстро: молния дважды коротко «вздрагивает» масштабом.
  fast: (t) => {
    const z = t == null ? 1 : 1 + 0.12 * bump(t, 0.08, 0.2) + 0.07 * bump(t, 0.22, 0.32);
    return tile('ft', C.amber, G.bolt, { transform: scaleAt(z) });
  },
  // Выгодно: пламя коротко качается от основания, огонёк внутри вспыхивает.
  fire: (t) =>
    tile('fr', C.amber, grp(A.swing(t, 7, 12, 20, 2, 0.06, 0.5), `<path d="${G.flame}"/>${grp(A.pop(t, 0.18, 12, 18), `<path d="${G.flameCore}"/>`)}`), {
      sw: 1.7,
    }),
  // Пламя для текста (выбор владельца 9 ноября 2026, вариант А): огонёк «Выгодно» без плитки —
  // оранжевая заливка пламени, белый контур и огонёк, как в плитке. В таблицах — плитка `fire`.
  flame: (t) => {
    // Контур — снизу, заливка — поверх: видна внешняя половина белой линии. Так пламя рисует и
    // Telegram (rlottie), и эталон — владелец выбирал по этому виду.
    const body = grp(A.swing(t, 7, 12, 20, 2, 0.06, 0.5), `<path d="${G.flame}"/><path d="${G.flame}" fill="${C.flame}" stroke="none"/>${grp(A.pop(t, 0.18, 12, 18), `<path d="${G.flameCore}"/>`)}`);
    return `<g transform="translate(12 12.4) scale(1.12) translate(-12 -12.1)"><g fill="none" stroke="${W}" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">${body}</g></g>`;
  },
  // Пауза: полоски по очереди коротко приседают.
  pause: (t) => tile('ps', C.slate, PAUSE(t), { sw: 2.15 }),
  ban: (t) => tile('bn', C.coral, `${G.banRing}${grp(A.swing(t, 14), G.banSlash)}`),
  // Тревога: от плитки расходятся квадратные волны — критическое, нужен человек. Всю петлю.
  alarm: (t) => {
    let rings = '';
    let s = 1;
    if (t != null) {
      for (const off of [0, 0.5]) rings += ripple(C.coral, ((t - off + 1) % 1) / 0.6, 5.8, 5.4);
      s = 1 - 0.07 * Math.sin(2 * Math.PI * 2 * t);
    }
    const r = 6.2 * s;
    return `${rings}<rect x="${n(12 - r)}" y="${n(12 - r)}" width="${n(2 * r)}" height="${n(2 * r)}" rx="${n(r * 0.42)}" fill="${C.coral}"/>${glyph(
      `<path d="M12 9.2v3.2"/><circle cx="12" cy="15" r=".9" fill="${W}" stroke="none"/>`,
      { sw: 1.8, scale: 1 },
    )}`;
  },
  // На связи: мятная точка, от неё спокойная квадратная волна.
  live: (t) => {
    const ring = t == null ? '' : ripple(C.mint, seg(t, 0, 0.8), 4.4, 6);
    return `${ring}<rect x="7.4" y="7.4" width="9.2" height="9.2" rx="3.9" fill="${C.mint}"/>`;
  },
  robot: (t) => tile('rb', C.blue, `${G.robotBody}${grp(A.blink(t, 13, 0.16), G.robotEyes)}`, { sw: 1.75 }),
  vault: (t) => tile('vt', C.mint, `${G.vaultBox}${grp(A.turn(t, 90), G.vaultDial)}`, { sw: 1.75 }),
  // Лимит Telegram (FLOOD_WAIT): песочные часы переворачиваются.
  flood: (t) => tile('fl', C.amber, G.hourglass, { transform: A.turn(t, 180, 12, 12, 0.06, 0.46), sw: 1.75 }),
  kill: (t) => tile('kl', C.coral, G.power, { transform: A.pop(t, 0.1), sw: 2 }),
  maint: (t) => tile('mt', C.slate, G.wrench, { transform: A.swing(t, 18), sw: 1.75 }),
  broadcast: (t) => {
    const wave = t == null ? '' : scaleAt(1 + 0.22 * bump(t, 0.12, 0.36), 15.6, 12);
    return tile('bc', C.sky, `${G.megaphone}${grp(wave, G.megaWave)}`, { transform: A.shake(t, 0.5, 3, 0.06, 0.34), sw: 1.75 });
  },
  limit: (t) => tile('lm', C.blue, `${G.gaugeArc}${grp(A.swing(t, -38, 12, 13.6, 1, 0.06, 0.5), G.gaugeNeedle)}`, { sw: 1.75 }),
  // Праздник: плитка с галочкой «подпрыгивает», вокруг разлетаются и сжимаются искры.
  confetti: (t) => {
    const pop = t == null ? 1 : 1 + 0.12 * bump(t, 0, 0.16) - 0.04 * bump(t, 0.16, 0.28);
    const cols = [C.amber, C.blue, C.coral, C.mint, C.violet, C.sky];
    let conf = '';
    for (let i = 0; i < 6; i++) {
      const ang = (i / 6) * 2 * Math.PI - Math.PI / 2 + 0.3;
      const p = t == null ? 0.6 : clamp(t / 0.55);
      if (t != null && (p <= 0.001 || p >= 0.999)) continue;
      const r = 5.6 + 5 * easeOut(p);
      const k = t == null ? 1 : 1 - Math.pow(p, 1.8);
      if (k < 0.04) continue;
      const x = 12 + r * Math.cos(ang);
      const y = 12 + r * Math.sin(ang);
      conf += i % 2 ? `<circle cx="${n(x)}" cy="${n(y)}" r="${n(1.05 * k)}" fill="${cols[i]}"/>` : `<path d="${SPARK(x, y, 1.7 * k)}" fill="${cols[i]}"/>`;
    }
    return `${conf}<g transform="translate(12 12) scale(${n(0.64 * pop)}) translate(-12 -12)">${tileBg('cf', C.mint)}${glyph(`<path d="${G.check}"/>`, { sw: 2.1 })}</g>`;
  },
  // Шаги заявки.
  step_done: (t) => `<circle cx="12" cy="12" r="8.6" fill="${C.mint}"/>${glyph(`<path d="${G.check}"/>`, { sw: 2.2, scale: 0.78, transform: A.pop(t, 0.12) })}`,
  step_now: (t) => {
    const s = t == null ? 1 : 1 - 0.18 * bump(t, 0, 0.5);
    return `<circle cx="12" cy="12" r="7.9" fill="none" stroke="${C.blue}" stroke-width="2"/><circle cx="12" cy="12" r="${n(3.9 * s)}" fill="${C.blue}"/>`;
  },
  step_todo: (t) => {
    const s = t == null ? 1 : 1 - 0.08 * bump(t, 0.06, 0.4);
    return `<circle cx="12" cy="12" r="${n(7.9 * s)}" fill="none" stroke="${C.grey}" stroke-width="1.9"/>`;
  },
  // Точки статусов.
  dot_ok: (t) => DOT(C.mint, t),
  dot_work: (t) => DOT(C.blue, t, 0.12, true),
  dot_wait: (t) => {
    const ring = t == null ? '' : ripple(C.amber, seg(t, 0, 0.8), 5.8, 4.6);
    return `${ring}<rect x="6.2" y="6.2" width="11.6" height="11.6" rx="4.9" fill="${C.amber}"/>`;
  },
  dot_act: (t) => DOT(C.coral, t, 0.16, true),
  dot_closed: (t) => DOT(C.grey, t, 0.08),
  // Кольца прогресса вместо шкал: рассылка, установка набора, самопроверка.
  prog_25: PROG(0.25),
  prog_50: PROG(0.5),
  prog_75: PROG(0.75),
};

// ── Линейные значки (перекрашиваются Telegram) ───────────────────────────────────────
const DOT_F = (cx, cy, r = 1) => `<circle cx="${cx}" cy="${cy}" r="${r}" fill="#fff" stroke="none"/>`;
const B = {
  invoice: `<path d="${G.receipt}"/>${G.receiptLines.map((d) => `<path d="${d}"/>`).join('')}`,
  help: `<circle cx="12" cy="12" r="8.6"/>`,
  helpMark: `<path d="M9.7 9.5a2.4 2.4 0 0 1 4.6.9c0 1.6-2.3 2-2.3 3.4"/>${DOT_F(12, 16.6, 0.95)}`,
  copyBack: '<path d="M15.4 8.6V6.8a2.4 2.4 0 0 0-2.4-2.4H6.8a2.4 2.4 0 0 0-2.4 2.4V13a2.4 2.4 0 0 0 2.4 2.4h1.8"/>',
  copyFront: '<rect x="8.6" y="8.6" width="11" height="11" rx="2.4"/>',
  trayDown: '<path d="M5 15.8v1.6a2.4 2.4 0 0 0 2.4 2.4h9.2a2.4 2.4 0 0 0 2.4-2.4v-1.6"/>',
  arrowDown: '<path d="M12 4.4v10.2M7.8 10.6l4.2 4.2 4.2-4.2"/>',
  usdt: '<circle cx="12" cy="12" r="8.8"/><path d="M14.5 9.4c-.5-.9-1.4-1.4-2.5-1.4-1.5 0-2.5.7-2.5 1.8 0 2.6 5.2 1.5 5.2 4.2 0 1.2-1.1 2-2.7 2-1.3 0-2.2-.6-2.6-1.5M12 6.6v1.4M12 16v1.4"/>',
  home: '<path d="M4.6 11L12 4.8l7.4 6.2"/><path d="M6.6 9.6v8a1.6 1.6 0 0 0 1.6 1.6h7.6a1.6 1.6 0 0 0 1.6-1.6v-8"/><path d="M10.2 19.2v-4.4h3.6v4.4"/>',
  refresh: '<path d="M19 12a7 7 0 0 1-12.2 4.7M5 12a7 7 0 0 1 12.2-4.7"/><path d="M17.6 3.8v3.7h-3.7M6.4 20.2v-3.7h3.7"/>',
  gift: '<rect x="4.8" y="9" width="14.4" height="3.6" rx="1.2"/><path d="M6.2 12.6v5.2a1.6 1.6 0 0 0 1.6 1.6h8.4a1.6 1.6 0 0 0 1.6-1.6v-5.2M12 9v10.4"/><path d="M12 9c-1.4-3.4-5-3.6-5-1.6 0 1.6 3.4 1.6 5 1.6zm0 0c1.4-3.4 5-3.6 5-1.6 0 1.6-3.4 1.6-5 1.6z"/>',
  star: '<path d="M12 4.4l2.3 4.7 5.1.7-3.7 3.6.9 5.1-4.6-2.4-4.6 2.4.9-5.1-3.7-3.6 5.1-.7z"/>',
  globe: '<circle cx="12" cy="12" r="8.4"/><path d="M3.8 12h16.4M12 3.6c2.2 2.3 3.3 5.1 3.3 8.4s-1.1 6.1-3.3 8.4c-2.2-2.3-3.3-5.1-3.3-8.4s1.1-6.1 3.3-8.4z"/>',
  linkA: '<path d="M10.4 13.6a3.6 3.6 0 0 0 5.1 0l2.6-2.6a3.6 3.6 0 0 0-5.1-5.1l-1 1"/>',
  linkB: '<path d="M13.6 10.4a3.6 3.6 0 0 0-5.1 0L5.9 13a3.6 3.6 0 0 0 5.1 5.1l1-1"/>',
  dash: '<rect x="4.4" y="4.4" width="6.6" height="8" rx="1.8"/><rect x="13" y="4.4" width="6.6" height="5" rx="1.8"/><rect x="13" y="11.4" width="6.6" height="8.2" rx="1.8"/><rect x="4.4" y="14.4" width="6.6" height="5.2" rx="1.8"/>',
  order: '<path d="M7.2 4.4h6.4l4.6 4.6v9a2.4 2.4 0 0 1-2.4 2.4H7.2A2.4 2.4 0 0 1 4.8 18V6.8a2.4 2.4 0 0 1 2.4-2.4z"/><path d="M13.4 4.6V9h4.4M8.4 13h7.2M8.4 16.4h4.6"/>',
  user: '<circle cx="12" cy="8.4" r="3.6"/><path d="M5.2 19.4a6.8 6.8 0 0 1 13.6 0"/>',
  users: '<circle cx="9.4" cy="8.6" r="3.2"/><path d="M3.8 19a5.6 5.6 0 0 1 11.2 0"/><path d="M15.4 5.6a3.2 3.2 0 0 1 0 6M17.8 13.8a5.6 5.6 0 0 1 2.6 5.2"/>',
  search: '<circle cx="10.8" cy="10.8" r="6.2"/><path d="M15.4 15.4l4.2 4.2"/>',
  filter: '<path d="M4.4 6h15.2l-5.8 6.8v5.4l-3.6 1.8v-7.2z"/>',
  settingsLines: '<path d="M4.6 7h9.2M17.8 7h1.6M4.6 12h3.2M11.8 12h7.6M4.6 17h7.6M16.2 17h3.2"/>',
  bell: '<path d="M6.6 10.2a5.4 5.4 0 0 1 10.8 0c0 4.9 2 6.6 2 6.6H4.6s2-1.7 2-6.6"/><path d="M10.4 19.6a1.9 1.9 0 0 0 3.2 0"/>',
  journal: '<path d="M5.6 5.4a1.6 1.6 0 0 1 1.6-1.6h10.6v15.4H7.2a1.6 1.6 0 0 0-1.6 1.6z"/><path d="M5.6 18.8V20M9.4 8.2h5M9.4 11.4h5"/>',
  chartAxis: '<path d="M4.4 4.6v13a2 2 0 0 0 2 2h13.2"/>',
  exportBox: '<path d="M17.4 14v3.2a2.4 2.4 0 0 1-2.4 2.4H7a2.4 2.4 0 0 1-2.4-2.4V9.2A2.4 2.4 0 0 1 7 6.8h3.2"/>',
  exportArrow: '<path d="M13.4 4.6h6v6M19.4 4.6l-8 8"/>',
  emojiFace: '<circle cx="12" cy="12" r="8.4"/><path d="M8.6 13.8a4.2 4.2 0 0 0 6.8 0"/>',
  emojiEyes: `${DOT_F(9.3, 9.8, 1.05)}${DOT_F(14.7, 9.8, 1.05)}`,
  staff: '<rect x="5" y="4.6" width="14" height="15" rx="2.6"/><circle cx="12" cy="10.2" r="2.6"/><path d="M8.4 16.6a3.8 3.8 0 0 1 7.2 0M10 4.6v1.6h4V4.6"/>',
  key: '<circle cx="8.4" cy="15.6" r="3.6"/><path d="M11 13l7.6-7.6M16.4 7.6l2.2 2.2M14.2 9.8l1.8 1.8"/>',
  play: '<path d="M8.6 6.4v11.2l9-5.6z"/>',
  edit: '<path d="M15.4 5.2a2.1 2.1 0 0 1 3 3L9 17.6l-4 1 1-4z"/><path d="M13.8 6.8l3 3"/>',
  plus: '<path d="M12 5.6v12.8M5.6 12h12.8"/>',
  trashLid: '<path d="M4.8 7h14.4M9.6 7V5.4a1 1 0 0 1 1-1h2.8a1 1 0 0 1 1 1V7"/>',
  trashCan: '<path d="M6.8 7l.8 11.2a1.8 1.8 0 0 0 1.8 1.6h5.2a1.8 1.8 0 0 0 1.8-1.6L17.2 7"/>',
  calendar: '<rect x="4.6" y="5.6" width="14.8" height="14" rx="2.6"/><path d="M4.6 10h14.8M8.6 3.8v3.4M15.4 3.8v3.4"/>',
  alert: `<path d="M10.5 5.2a1.7 1.7 0 0 1 3 0l6.2 11a1.7 1.7 0 0 1-1.5 2.6H5.8a1.7 1.7 0 0 1-1.5-2.6z"/><path d="M12 9.6v3.4"/>${DOT_F(12, 15.8, 0.95)}`,
  flag: '<path d="M6.2 20V4.8"/><path d="M6.2 5.2c4-2 7.4 2 11.6 0v8.4c-4.2 2-7.6-2-11.6 0"/>',
  pin: '<path d="M12 15.6v4.6"/><path d="M8.6 4.6h6.8M9.6 4.6v4.6l-2.6 3v1.4h10v-1.4l-2.6-3V4.6"/>',
  topic: '<path d="M5 6.8a2 2 0 0 1 2-2h10a2 2 0 0 1 2 2v7.4a2 2 0 0 1-2 2h-6.4L7 19.2v-3H7a2 2 0 0 1-2-2z"/><path d="M8.6 9h6.8M8.6 12h4.4"/>',
  ack: '<path d="M4.8 12.6l3.6 3.6 7.6-8"/><path d="M12.8 15.4l.8.8 7.6-8"/>',
  take: '<path d="M8.6 11V6.4a1.6 1.6 0 0 1 3.2 0V11M11.8 10.2V5.2a1.6 1.6 0 0 1 3.2 0v5M15 10.2V6.8a1.6 1.6 0 0 1 3.2 0v6.6a6.2 6.2 0 0 1-6.2 6.2h-.6a6.2 6.2 0 0 1-5.2-2.8l-2-3.2a1.6 1.6 0 0 1 2.6-1.8l1 1.2"/>',
  coin: '<circle cx="9.6" cy="9.6" r="5.4"/><path d="M15.6 9.4a5.4 5.4 0 1 1-6.2 6.2"/>',
  phone: '<rect x="7.2" y="3.8" width="9.6" height="16.4" rx="2.4"/><path d="M10.8 17.2h2.4"/>',
  plug: '<path d="M9 3.8v4M15 3.8v4"/><path d="M6.6 7.8h10.8v2.6a5.4 5.4 0 0 1-10.8 0z"/><path d="M12 15.8v4.4"/>',
  inbox: '<path d="M4.4 13.2l2.6-7a1.6 1.6 0 0 1 1.5-1h7a1.6 1.6 0 0 1 1.5 1l2.6 7v4.4a1.6 1.6 0 0 1-1.6 1.6H6a1.6 1.6 0 0 1-1.6-1.6z"/><path d="M4.4 13.2h4.2l1.2 2.2h4.4l1.2-2.2h4.2"/>',
  serverBoxes: '<rect x="4.6" y="4.6" width="14.8" height="6" rx="1.8"/><rect x="4.6" y="13.4" width="14.8" height="6" rx="1.8"/><path d="M12.4 7.6h3.4M12.4 16.4h3.4"/>',
  wallet: '<path d="M5 7.6a2.2 2.2 0 0 1 2.2-2.2h9.4v2.8"/><rect x="5" y="8.2" width="14.4" height="11" rx="2.4"/><path d="M19.4 11.6h-3a2 2 0 0 0 0 4h3"/>',
};

// Каждый моно-значок — (t) => тело внутри общей группы с обводкой MONO_W.
const MONO = {
  // Клиент
  swap: (t) => grp(A.turn(t, 180, 12, 12, 0.08, 0.5), G.swap),
  ticket: (t) => grp(A.swing(t, 8), G.ticket),
  invoice: (t) => grp(A.nudge(t, 0, -1.2), B.invoice),
  rates: (t) => grp(A.turn(t, 180), G.percent),
  history: (t) => `${G.historyArc}${grp(A.spin(t, -360), G.historyHands)}`,
  support: (t) => grp(A.pop(t, 0.09, 12, 18), G.chat),
  help: (t) => `${B.help}${grp(A.swing(t, 12, 12, 13), B.helpMark)}`,
  copy: (t) => `${B.copyBack}${grp(A.nudge(t, -1.2, -1.2), B.copyFront)}`,
  claim: (t) => `${B.trayDown}${grp(A.nudge(t, 0, 1.6), B.arrowDown)}`,
  cb: (t) => grp(A.spin(t, 360, 12, 12, 0.06, 0.62), G.orbit),
  xr: (t) => grp(A.nudge(t, 1.1, -1.1, 0.06, 0.36), `<g transform="rotate(45 12 12)">${G.rocket}</g>`),
  usdt: (t) => grp(A.swing(t, 12), B.usdt),
  done: (t) => `<circle cx="12" cy="12" r="8.8"/>${grp(A.pop(t, 0.14), '<path d="M8.2 12.3l2.6 2.6 5-5.2"/>')}`,
  close: (t) => grp(A.turn(t, 90), '<path d="M7.4 7.4l9.2 9.2M16.6 7.4l-9.2 9.2"/>'),
  back: (t) => grp(A.nudge(t, -1.6, 0), '<path d="M19 12H5.6M10.6 7l-5 5 5 5"/>'),
  down: (t) => {
    const dy = t == null ? 0 : 1.7 * bump(t, 0.06, 0.28) + 0.9 * bump(t, 0.28, 0.46);
    return grp(move(0, dy), `<path d="${DOWN.d}"/>`);
  },
  home: (t) => grp(A.pop(t, 0.08, 12, 19), B.home),
  refresh: (t) => grp(A.spin(t, 360), B.refresh),
  refund: (t) => grp(A.nudge(t, -1.5, 0), `<path d="${G.uturn}"/><path d="${G.uturnHead}"/>`),
  wait: (t) => {
    const a = t == null ? 0 : 360 * t;
    return `${G.clockFace}<path d="M12 12l2.9 1.7"/>${grp(Math.abs(a % 360) < 0.01 ? '' : rotAt(a % 360), '<path d="M12 12V7.4"/>')}`;
  },
  spin: (t) => {
    const a = t == null ? 0 : 720 * t;
    return grp(Math.abs(a % 360) < 0.01 ? '' : rotAt(a % 360), '<path d="M12 4.2a7.8 7.8 0 1 1-7.8 7.8"/>');
  },
  fast: (t) => grp(t == null ? '' : scaleAt(1 + 0.12 * bump(t, 0.08, 0.2) + 0.07 * bump(t, 0.22, 0.32)), '<path d="M13.2 4L7 13.2h5l-1.2 6.8 6.2-9.2h-5z"/>'),
  gift: (t) => grp(A.swing(t, 7, 12, 19), B.gift),
  star: (t) => grp(A.pop(t, 0.12, 12, 12.6), B.star),
  globe: (t) => grp(A.swing(t, 14), B.globe),
  link: (t) => `${grp(A.nudge(t, 0.9, -0.9), B.linkA)}${grp(A.nudge(t, -0.9, 0.9), B.linkB)}`,
  // Админка
  dash: (t) => grp(A.pop(t, 0.08), B.dash),
  queue: (t) =>
    [6.6, 12, 17.4]
      .map((y, i) => grp(A.nudge(t, 1.4, 0, 0.06 + i * 0.06, 0.3 + i * 0.06), `<path d="M9.4 ${y}h10"/>${DOT_F(5.4, y, 1.05)}`))
      .join(''),
  order: (t) => grp(A.nudge(t, 0, -1.2), B.order),
  user: (t) => grp(A.nudge(t, 0, -1.1), B.user),
  users: (t) => grp(A.nudge(t, 0, -1.1), B.users),
  ban: (t) => `${G.banRing}${grp(A.swing(t, 14), G.banSlash)}`,
  search: (t) => {
    const k = t == null ? 0 : seg(t, 0.06, 0.5);
    const tr = t == null ? '' : move(1.3 * Math.sin(2 * Math.PI * k) * (1 - k * 0.2) * Math.sin(Math.PI * k), 0.9 * Math.sin(4 * Math.PI * k) * Math.sin(Math.PI * k));
    return grp(tr, B.search);
  },
  filter: (t) => grp(A.nudge(t, 0, 1.2), B.filter),
  settings: (t) =>
    `${B.settingsLines}${[
      [15.8, 7, -1.6],
      [9.8, 12, 1.8],
      [14.2, 17, -1.4],
    ]
      .map(([x, y, dx], i) => grp(A.nudge(t, dx, 0, 0.06 + i * 0.07, 0.32 + i * 0.07), `<circle cx="${x}" cy="${y}" r="2"/>`))
      .join('')}`,
  bell: (t) => grp(A.swing(t, 14, 12, 4.8, 2), B.bell),
  vault: (t) => `${G.vaultBox}${grp(A.turn(t, 90), G.vaultDial)}`,
  robot: (t) => `${G.robotBody}${grp(A.blink(t, 13, 0.16), G.robotEyes)}`,
  megaphone: (t) => {
    const wave = t == null ? '' : scaleAt(1 + 0.22 * bump(t, 0.12, 0.36), 15.6, 12);
    return grp(A.shake(t, 0.5, 3, 0.06, 0.34), `${G.megaphone}${grp(wave, G.megaWave)}`);
  },
  journal: (t) => grp(A.nudge(t, 0, -1.1), B.journal),
  // Столбцы по очереди опускаются и вырастают обратно.
  chart: (t) =>
    `${B.chartAxis}${[
      [8.6, 12.2],
      [12.6, 8.4],
      [16.6, 10.4],
    ]
      .map(([x, top], i) => `<path d="M${x} 15.4V${n(top + (15.4 - top) * 0.45 * press(t, 0.06 + i * 0.07, 0.34 + i * 0.07))}"/>`)
      .join('')}`,
  export: (t) => `${B.exportBox}${grp(A.nudge(t, 1.2, -1.2), B.exportArrow)}`,
  emoji: (t) => `${B.emojiFace}${grp(A.blink(t, 9.8, 0.2), B.emojiEyes)}`,
  staff: (t) => grp(A.swing(t, 8, 12, 4.6), B.staff),
  key: (t) => grp(A.swing(t, 22, 8.4, 15.6, 1), B.key),
  power: (t) => grp(A.pop(t, 0.1), G.power),
  pause: (t) => PAUSE(t),
  play: (t) => grp(A.nudge(t, 1.4, 0), B.play),
  maint: (t) => grp(A.swing(t, 18), G.wrench),
  shield: (t) => grp(A.pop(t, 0.08), `<path d="${G.shield}"/><path d="${G.shieldCheck}"/>`),
  lock: (t) => `${G.lockBody}${grp(A.nudge(t, 0, -1.3, 0.06, 0.32), G.lockShackle)}`,
  // Моргает геометрией: веки сходятся, зрачок уменьшается и прячется.
  eye: (t) => {
    const h = n(5.8 * (1 - 0.86 * press(t, 0.16, 0.28)));
    const r = Math.min(2.6, 0.75 * h - 0.3);
    return `<path d="M3.6 12s3-${h} 8.4-${h} 8.4 ${h} 8.4 ${h}-3 ${h}-8.4 ${h}S3.6 12 3.6 12z"/>${r > 0.5 ? `<circle cx="12" cy="12" r="${n(r)}"/>` : ''}`;
  },
  edit: (t) => grp(A.swing(t, 9, 5.4, 18.4), B.edit),
  plus: (t) => grp(A.turn(t, 90), B.plus),
  minus: (t) => {
    const l = 6.4 * (1 - 0.35 * press(t, 0.06, 0.32));
    return `<path d="M${n(12 - l)} 12h${n(2 * l)}"/>`;
  },
  trash: (t) => {
    const lid = t == null ? '' : [move(0, -1.2 * bump(t, 0.06, 0.34)), rotAt(-9 * bump(t, 0.06, 0.34), 4.8, 7)].filter(Boolean).join(' ');
    return `${grp(lid, B.trashLid)}${B.trashCan}`;
  },
  calendar: (t) => grp(A.nudge(t, 0, -1.1), B.calendar),
  limit: (t) => `${G.gaugeArc}${grp(A.swing(t, -38, 12, 13.6, 1, 0.06, 0.5), G.gaugeNeedle)}`,
  alert: (t) => grp(A.shake(t, 1), B.alert),
  flag: (t) => grp(A.swing(t, 7, 6.2, 20), B.flag),
  pin: (t) => grp(A.nudge(t, 0, 1.3), B.pin),
  topic: (t) => grp(A.pop(t, 0.09, 12, 18), B.topic),
  ack: (t) => grp(A.pop(t, 0.12), B.ack),
  take: (t) => grp(A.swing(t, 10, 12, 19.6), B.take),
  coin: (t) => grp(A.swing(t, 12, 12, 12.6), B.coin),
  sparkle: (t) => `${grp(A.turn(t, 90, 10.6, 12.6), `<path d="${SPARK(10.6, 12.6, 7.6)}"/>`)}${grp(A.pop(t, 0.3, 18.4, 5.6), '<path d="M18.4 3.8v3.6M16.6 5.6h3.6"/>')}`,
  // Юзерботы и сервис
  phone: (t) => grp(A.swing(t, 9, 12, 12, 3, 0.06, 0.42), B.phone),
  plug: (t) => grp(A.nudge(t, 0, -1.4), B.plug),
  inbox: (t) => grp(A.nudge(t, 0, 1.1), B.inbox),
  server: (t) =>
    `${B.serverBoxes}${[7.6, 16.4]
      .map((y, i) => grp(t == null ? '' : scaleAt(1 + 0.6 * bump(t, 0.08 + i * 0.12, 0.24 + i * 0.12), 8, y), DOT_F(8, y, 0.95)))
      .join('')}`,
  wallet: (t) => grp(A.pop(t, 0.08), B.wallet),
  // Админка: выгодный курс, встречный поток, ступени комиссии.
  fire: (t) => grp(A.swing(t, 7, 12, 20, 2, 0.06, 0.5), `<path d="${G.flame}"/>${grp(A.pop(t, 0.18, 12, 18), `<path d="${G.flameCore}"/>`)}`),
  scale: (t) => `${G.scalePost}${grp(A.swing(t, 9, 12, 7.6, 1, 0.06, 0.6), G.scaleBeam)}`,
  tiers: (t) => `${G.stairs}${grp(A.nudge(t, 1.1, -1.1, 0.06, 0.34), G.stairsArrow)}`,
};

// Подписи — для превью-листа и админки «Эмодзи».
const LABELS = {
  done: 'Готово', swap: 'Обмен', process: 'В работе', wait: 'Ждём', err: 'Ошибка',
  warn: 'Внимание', refund: 'Возврат', manual: 'Ручная проверка', invoice: 'Счёт', sending: 'Чек в пути', usdt: 'USDT',
  cb: 'CryptoBot', xr: 'xRocket', shield: 'Защита', lock: 'Приватность', rates: 'Курс', history: 'История',
  support: 'Поддержка', fast: 'Быстро', pause: 'Пауза', ban: 'Бан', alarm: 'Тревога', live: 'На связи',
  robot: 'Юзербот', vault: 'Резерв', flood: 'Лимит Telegram', kill: 'Стоп-кран', maint: 'Обслуживание',
  broadcast: 'Рассылка', limit: 'Лимиты', confetti: 'Первый обмен', step_done: 'Шаг: готов', step_now: 'Шаг: сейчас',
  step_todo: 'Шаг: впереди', dot_ok: 'Хорошо', dot_work: 'В работе', dot_wait: 'Ждём', dot_act: 'Нужен человек',
  dot_closed: 'Закрыто', prog_25: 'Прогресс 25 %', prog_50: 'Прогресс 50 %', prog_75: 'Прогресс 75 %',
  down: 'Вниз', ticket: 'Чек', help: 'Помощь', copy: 'Копировать', claim: 'Забрать', close: 'Закрыть', back: 'Назад', home: 'Главная',
  refresh: 'Обновить', spin: 'Загрузка', gift: 'Подарок', star: 'Звезда', globe: 'Язык', link: 'Ссылка', dash: 'Сводка',
  queue: 'Очередь', order: 'Заявка', user: 'Клиент', users: 'Клиенты', search: 'Поиск', filter: 'Фильтр',
  settings: 'Настройки', bell: 'Алерты', megaphone: 'Рассылка', journal: 'Журнал', chart: 'Статистика',
  export: 'Экспорт', emoji: 'Эмодзи', staff: 'Персонал', key: 'Ключи', power: 'Стоп-кран', play: 'Пуск',
  eye: 'Смотреть', edit: 'Изменить', plus: 'Добавить', minus: 'Убрать', trash: 'Удалить', calendar: 'Период',
  alert: 'Внимание', flag: 'Отметить', pin: 'Закрепить', topic: 'Тема', ack: 'Принял', take: 'Взять',
  coin: 'Деньги', sparkle: 'Новое', phone: 'Телефон', plug: 'Подключить', inbox: 'Обращения', server: 'Система',
  wallet: 'Кошелёк', fire: 'Выгодно', flame: 'Выгодно (в тексте)', scale: 'Встречный поток', tiers: 'Ступени',
};

module.exports = { C, COLOR, MONO, LABELS, MONO_W };
