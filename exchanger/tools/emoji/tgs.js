// Векторные эмодзи exch: значки из icons.js → Lottie → .tgs. Конвертер перенесён из инструментов
// «Реборна» (тот же владелец; tools/emoji/tgs.js). Отличие одно: толщина обводки не входит в
// подпись фигуры, поэтому её можно анимировать (волна истончается, а не тает).
//
// Растровые кастомные эмодзи Telegram принимает только 100×100 — при крупном показе и на
// плотных экранах они мылятся. TGS — вектор (Lottie), Telegram рисует его под экран, и
// значок чёткий в любом размере. Требования Telegram к TGS: холст 512×512, 60 кадров/с,
// не дольше 3 с, файл до 64 КБ; без масок, эффектов, текста, merge paths, градиентных
// обводок и выражений.
//
// Как устроено. Значок — SVG (для анимированных — функция времени). Каждый кадр
// разбирается на фигуры: путь в кубических Безье, заливка или обводка, прозрачность,
// матрица преобразования, обрезка (clipPath). Фигуры сопоставляются между кадрами, и для
// каждой подбирается самое лёгкое описание движения: если меняется только положение,
// поворот или масштаб — геометрия хранится один раз, а анимируется преобразование группы
// с якорем в неподвижной точке; иначе — ключевые кадры самой геометрии. Ключевые кадры
// прорежены (Рамер — Дуглас — Пекер) с допуском меньше трети пикселя на холсте 512.
// Обрезка по кругу или щиту (блик) — трек-маттом (маски Telegram запрещает), текст
// (числа на щитах) — контурами шрифта DejaVu Sans Bold, пунктир — штрихами обводки,
// бегущая дуга — trim paths.
//
//   (cd tools/emoji && npm ci)
//   node tools/emoji/tgs.js assets/emoji [ключ,ключ]   — порядок проверки: tools/emoji/README.md

const fs = require('fs');
const path = require('path');
const zlib = require('zlib');
const { COLOR, MONO } = require('./icons');

const SIZE = 512;
const FPS = 60;
const SECONDS = 2.8;
const FRAMES = Math.round(FPS * SECONDS); // 168
// Статичный значок — секундная петля одинаковых кадров: Telegram ждёт анимацию.
const STATIC_FRAMES = 60;
const LIMIT = 64 * 1024;
const KAPPA = 0.5522847498307936;

const FONT_FILE = process.env.EXCH_FONT || '/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf';
let FONT = null;
function font() {
  if (!FONT) FONT = require('opentype.js').loadSync(FONT_FILE);
  return FONT;
}

// ── Матрицы: [a, b, c, d, e, f] — x' = a·x + c·y + e, y' = b·x + d·y + f ─────────
const I = [1, 0, 0, 1, 0, 0];
const mul = (m, n) => [
  m[0] * n[0] + m[2] * n[1],
  m[1] * n[0] + m[3] * n[1],
  m[0] * n[2] + m[2] * n[3],
  m[1] * n[2] + m[3] * n[3],
  m[0] * n[4] + m[2] * n[5] + m[4],
  m[1] * n[4] + m[3] * n[5] + m[5],
];
const inv = (m) => {
  const det = m[0] * m[3] - m[1] * m[2];
  return [m[3] / det, -m[1] / det, -m[2] / det, m[0] / det, (m[2] * m[5] - m[3] * m[4]) / det, (m[1] * m[4] - m[0] * m[5]) / det];
};
const apply = (m, [x, y]) => [m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5]];
const scaleOf = (m) => Math.sqrt(Math.abs(m[0] * m[3] - m[1] * m[2]));

function parseTransform(s) {
  let m = I;
  if (!s) return m;
  const re = /(\w+)\s*\(([^)]*)\)/g;
  let t;
  while ((t = re.exec(s))) {
    const v = t[2].trim().split(/[\s,]+/).filter(Boolean).map(Number);
    let k;
    switch (t[1]) {
      case 'translate':
        k = [1, 0, 0, 1, v[0], v[1] || 0];
        break;
      case 'scale':
        k = [v[0], 0, 0, v.length > 1 ? v[1] : v[0], 0, 0];
        break;
      case 'rotate': {
        const r = (v[0] * Math.PI) / 180;
        const [cx, cy] = [v[1] || 0, v[2] || 0];
        k = mul(mul([1, 0, 0, 1, cx, cy], [Math.cos(r), Math.sin(r), -Math.sin(r), Math.cos(r), 0, 0]), [1, 0, 0, 1, -cx, -cy]);
        break;
      }
      case 'matrix':
        k = v;
        break;
      default:
        throw new Error(`transform ${t[1]} не поддержан`);
    }
    m = mul(m, k);
  }
  return m;
}

// ── Мини-разбор SVG (значки генерируются нами, разметка простая и правильная) ───────
function parseSvg(src) {
  const root = { tag: 'root', attrs: {}, children: [] };
  const stack = [root];
  const re = /<(\/?)([a-zA-Z]+)((?:\s+[\w:-]+="[^"]*")*)\s*(\/?)>|([^<]+)/g;
  let m;
  while ((m = re.exec(src))) {
    if (m[5] !== undefined) {
      if (m[5].trim()) stack[stack.length - 1].children.push({ tag: '#text', text: m[5] });
      continue;
    }
    const [, close, tag, rawAttrs, self] = m;
    if (close) {
      stack.pop();
      continue;
    }
    const attrs = {};
    for (const a of rawAttrs.matchAll(/([\w:-]+)="([^"]*)"/g)) attrs[a[1]] = a[2];
    const node = { tag, attrs, children: [] };
    stack[stack.length - 1].children.push(node);
    if (!self) stack.push(node);
  }
  return root.children[0];
}

// ── Пути → подпути из кубических сегментов ─────────────────────────────────────────
// Подпуть: { start: [x, y], segs: [[c1x, c1y, c2x, c2y, x, y], …], closed }
function arcToCubics(x1, y1, rx, ry, phi, fa, fs, x2, y2) {
  const out = [];
  if (rx === 0 || ry === 0) return [[x1, y1, x2, y2, x2, y2]];
  const sinp = Math.sin((phi * Math.PI) / 180);
  const cosp = Math.cos((phi * Math.PI) / 180);
  const dx = (x1 - x2) / 2;
  const dy = (y1 - y2) / 2;
  const x1p = cosp * dx + sinp * dy;
  const y1p = -sinp * dx + cosp * dy;
  rx = Math.abs(rx);
  ry = Math.abs(ry);
  const lam = (x1p * x1p) / (rx * rx) + (y1p * y1p) / (ry * ry);
  if (lam > 1) {
    rx *= Math.sqrt(lam);
    ry *= Math.sqrt(lam);
  }
  const num = rx * rx * ry * ry - rx * rx * y1p * y1p - ry * ry * x1p * x1p;
  const den = rx * rx * y1p * y1p + ry * ry * x1p * x1p;
  let co = Math.sqrt(Math.max(0, num / den));
  if (fa === fs) co = -co;
  const cxp = (co * rx * y1p) / ry;
  const cyp = (-co * ry * x1p) / rx;
  const cx = cosp * cxp - sinp * cyp + (x1 + x2) / 2;
  const cy = sinp * cxp + cosp * cyp + (y1 + y2) / 2;
  const ang = (ux, uy, vx, vy) => {
    const a = Math.atan2(ux * vy - uy * vx, ux * vx + uy * vy);
    return a;
  };
  const t1 = ang(1, 0, (x1p - cxp) / rx, (y1p - cyp) / ry);
  let dt = ang((x1p - cxp) / rx, (y1p - cyp) / ry, (-x1p - cxp) / rx, (-y1p - cyp) / ry);
  if (!fs && dt > 0) dt -= 2 * Math.PI;
  if (fs && dt < 0) dt += 2 * Math.PI;
  const n = Math.max(1, Math.ceil(Math.abs(dt) / (Math.PI / 2) - 1e-9));
  const step = dt / n;
  const k = (4 / 3) * Math.tan(step / 4);
  const pt = (t) => {
    const x = rx * Math.cos(t);
    const y = ry * Math.sin(t);
    return [cosp * x - sinp * y + cx, sinp * x + cosp * y + cy];
  };
  const der = (t) => {
    const x = -rx * Math.sin(t);
    const y = ry * Math.cos(t);
    return [cosp * x - sinp * y, sinp * x + cosp * y];
  };
  for (let i = 0; i < n; i++) {
    const a = t1 + i * step;
    const b = a + step;
    const p0 = pt(a);
    const p3 = pt(b);
    const d0 = der(a);
    const d3 = der(b);
    out.push([p0[0] + k * d0[0], p0[1] + k * d0[1], p3[0] - k * d3[0], p3[1] - k * d3[1], p3[0], p3[1]]);
  }
  // Последняя точка — ровно конец дуги.
  out[out.length - 1][4] = x2;
  out[out.length - 1][5] = y2;
  return out;
}

function parsePathD(d) {
  const tokens = d.match(/[a-zA-Z]|-?(?:\d*\.\d+|\d+\.?)(?:e[-+]?\d+)?/g) || [];
  const subs = [];
  let cur = null;
  let x = 0;
  let y = 0;
  let sx = 0;
  let sy = 0;
  let prevC2 = null; // для S
  let prevQ = null; // для T
  let i = 0;
  let cmd = null;
  const num = () => Number(tokens[i++]);
  // Флаги дуги могут идти слитно: «a2 2 0 0110 10».
  const flag = () => {
    const t = tokens[i];
    if (t.length > 1 && (t[0] === '0' || t[0] === '1') && !t.includes('.')) {
      tokens[i] = t.slice(1);
      return Number(t[0]);
    }
    i++;
    return Number(t);
  };
  const line = (nx, ny) => {
    cur.segs.push([x, y, nx, ny, nx, ny]);
    x = nx;
    y = ny;
  };
  while (i < tokens.length) {
    if (/^[a-zA-Z]$/.test(tokens[i])) cmd = tokens[i++];
    const rel = cmd === cmd.toLowerCase();
    const C = cmd.toUpperCase();
    if (C === 'Z') {
      if (cur) {
        if (Math.hypot(x - sx, y - sy) > 1e-9) cur.segs.push([x, y, sx, sy, sx, sy]);
        cur.closed = true;
        x = sx;
        y = sy;
        cur = null;
      }
      prevC2 = prevQ = null;
      continue;
    }
    if (C === 'M') {
      const nx = num() + (rel ? x : 0);
      const ny = num() + (rel ? y : 0);
      cur = { start: [nx, ny], segs: [], closed: false };
      subs.push(cur);
      x = sx = nx;
      y = sy = ny;
      cmd = rel ? 'l' : 'L';
      prevC2 = prevQ = null;
      continue;
    }
    if (!cur) {
      cur = { start: [x, y], segs: [], closed: false };
      subs.push(cur);
      sx = x;
      sy = y;
    }
    switch (C) {
      case 'L':
        line(num() + (rel ? x : 0), num() + (rel ? y : 0));
        prevC2 = prevQ = null;
        break;
      case 'H':
        line(num() + (rel ? x : 0), y);
        prevC2 = prevQ = null;
        break;
      case 'V':
        line(x, num() + (rel ? y : 0));
        prevC2 = prevQ = null;
        break;
      case 'C': {
        const v = [num(), num(), num(), num(), num(), num()];
        if (rel) for (let k = 0; k < 6; k += 2) (v[k] += x), (v[k + 1] += y);
        cur.segs.push(v);
        prevC2 = [v[2], v[3]];
        prevQ = null;
        x = v[4];
        y = v[5];
        break;
      }
      case 'S': {
        const v = [num(), num(), num(), num()];
        if (rel) for (let k = 0; k < 4; k += 2) (v[k] += x), (v[k + 1] += y);
        const c1 = prevC2 ? [2 * x - prevC2[0], 2 * y - prevC2[1]] : [x, y];
        cur.segs.push([c1[0], c1[1], v[0], v[1], v[2], v[3]]);
        prevC2 = [v[0], v[1]];
        prevQ = null;
        x = v[2];
        y = v[3];
        break;
      }
      case 'Q':
      case 'T': {
        let q;
        let e;
        if (C === 'Q') {
          q = [num(), num()];
          e = [num(), num()];
          if (rel) (q[0] += x), (q[1] += y), (e[0] += x), (e[1] += y);
        } else {
          q = prevQ ? [2 * x - prevQ[0], 2 * y - prevQ[1]] : [x, y];
          e = [num(), num()];
          if (rel) (e[0] += x), (e[1] += y);
        }
        cur.segs.push([x + (2 / 3) * (q[0] - x), y + (2 / 3) * (q[1] - y), e[0] + (2 / 3) * (q[0] - e[0]), e[1] + (2 / 3) * (q[1] - e[1]), e[0], e[1]]);
        prevQ = q;
        prevC2 = null;
        x = e[0];
        y = e[1];
        break;
      }
      case 'A': {
        const rx = num();
        const ry = num();
        const phi = num();
        const fa = flag();
        const fsw = flag();
        const nx = num() + (rel ? x : 0);
        const ny = num() + (rel ? y : 0);
        for (const s of arcToCubics(x, y, rx, ry, phi, fa, fsw, nx, ny)) cur.segs.push(s);
        x = nx;
        y = ny;
        prevC2 = prevQ = null;
        break;
      }
      default:
        throw new Error(`команда пути ${cmd} не поддержана`);
    }
  }
  return subs.filter((s) => s.segs.length > 0);
}

// Круг — как в SVG: от (cx + r, cy) по часовой стрелке (y вниз), четыре дуги.
function circleSubs(cx, cy, r) {
  const k = KAPPA * r;
  return [
    {
      start: [cx + r, cy],
      segs: [
        [cx + r, cy + k, cx + k, cy + r, cx, cy + r],
        [cx - k, cy + r, cx - r, cy + k, cx - r, cy],
        [cx - r, cy - k, cx - k, cy - r, cx, cy - r],
        [cx + k, cy - r, cx + r, cy - k, cx + r, cy],
      ],
      closed: true,
    },
  ];
}

function rectSubs(x, y, w, h, rx, ry) {
  if (!rx && !ry) {
    return [{ start: [x, y], segs: [[x, y, x + w, y, x + w, y], [x + w, y, x + w, y + h, x + w, y + h], [x + w, y + h, x, y + h, x, y + h], [x, y + h, x, y, x, y]], closed: true }];
  }
  rx = Math.min(rx || ry, w / 2);
  ry = Math.min(ry || rx, h / 2);
  const kx = KAPPA * rx;
  const ky = KAPPA * ry;
  const L = (x0, y0, x1, y1) => [x0, y0, x1, y1, x1, y1];
  return [
    {
      start: [x + rx, y],
      segs: [
        L(x + rx, y, x + w - rx, y),
        [x + w - rx + kx, y, x + w, y + ry - ky, x + w, y + ry],
        L(x + w, y + ry, x + w, y + h - ry),
        [x + w, y + h - ry + ky, x + w - rx + kx, y + h, x + w - rx, y + h],
        L(x + w - rx, y + h, x + rx, y + h),
        [x + rx - kx, y + h, x, y + h - ry + ky, x, y + h - ry],
        L(x, y + h - ry, x, y + ry),
        [x, y + ry - ky, x + rx - kx, y, x + rx, y],
      ],
      closed: true,
    },
  ];
}

function textSubs(node, style) {
  const txt = node.children.filter((c) => c.tag === '#text').map((c) => c.text).join('').trim();
  const size = Number(node.attrs['font-size'] || style.fontSize || 16);
  const f = font();
  let x = Number(node.attrs.x || 0);
  const y = Number(node.attrs.y || 0);
  const adv = f.getAdvanceWidth(txt, size);
  const anchor = node.attrs['text-anchor'] || style.anchor || 'start';
  if (anchor === 'middle') x -= adv / 2;
  if (anchor === 'end') x -= adv;
  const p = f.getPath(txt, x, y, size);
  const subs = [];
  let cur = null;
  let cx = 0;
  let cy = 0;
  for (const c of p.commands) {
    if (c.type === 'M') {
      cur = { start: [c.x, c.y], segs: [], closed: false };
      subs.push(cur);
      cx = c.x;
      cy = c.y;
    } else if (c.type === 'L') {
      cur.segs.push([cx, cy, c.x, c.y, c.x, c.y]);
      cx = c.x;
      cy = c.y;
    } else if (c.type === 'Q') {
      cur.segs.push([cx + (2 / 3) * (c.x1 - cx), cy + (2 / 3) * (c.y1 - cy), c.x + (2 / 3) * (c.x1 - c.x), c.y + (2 / 3) * (c.y1 - c.y), c.x, c.y]);
      cx = c.x;
      cy = c.y;
    } else if (c.type === 'C') {
      cur.segs.push([c.x1, c.y1, c.x2, c.y2, c.x, c.y]);
      cx = c.x;
      cy = c.y;
    } else if (c.type === 'Z') {
      const [sx, sy] = cur.start;
      if (Math.hypot(cx - sx, cy - sy) > 1e-9) cur.segs.push([cx, cy, sx, sy, sx, sy]);
      cur.closed = true;
      cx = sx;
      cy = sy;
    }
  }
  return subs.filter((s) => s.segs.length);
}

// ── Пунктир → отдельные штрихи ─────────────────────────────────────────────────────
// Штрихи режем прямо в геометрии: у движков Lottie начало пунктира на замкнутом пути
// отличается от SVG, а готовые штрихи везде выглядят одинаково.
function cubicAt(p0, g, t) {
  const u = 1 - t;
  return [
    u * u * u * p0[0] + 3 * u * u * t * g[0] + 3 * u * t * t * g[2] + t * t * t * g[4],
    u * u * u * p0[1] + 3 * u * u * t * g[1] + 3 * u * t * t * g[3] + t * t * t * g[5],
  ];
}
// Кусок кубической кривой между параметрами t0..t1 (де Кастельжо).
function cubicPart(p0, g, t0, t1) {
  const split = (a, b, c, d, t) => {
    const lerp = (p, q) => [p[0] + (q[0] - p[0]) * t, p[1] + (q[1] - p[1]) * t];
    const ab = lerp(a, b);
    const bc = lerp(b, c);
    const cd = lerp(c, d);
    const abc = lerp(ab, bc);
    const bcd = lerp(bc, cd);
    const m = lerp(abc, bcd);
    return [[a, ab, abc, m], [m, bcd, cd, d]];
  };
  let pts = [p0, [g[0], g[1]], [g[2], g[3]], [g[4], g[5]]];
  if (t1 < 1) pts = split(...pts, t1)[0];
  if (t0 > 0) pts = split(...pts, t0 / t1)[1];
  return { start: pts[0], seg: [...pts[1], ...pts[2], ...pts[3]] };
}
function dashify(subs, pattern) {
  const pat = pattern.length % 2 ? [...pattern, ...pattern] : pattern;
  const out = [];
  for (const sub of subs) {
    // Таблица длины: по 64 отрезка на сегмент.
    const table = [];
    let len = 0;
    let p0 = sub.start;
    for (const g of sub.segs) {
      let prev = p0;
      const rows = [[0, len]];
      for (let k = 1; k <= 64; k++) {
        const pt = cubicAt(p0, g, k / 64);
        len += Math.hypot(pt[0] - prev[0], pt[1] - prev[1]);
        rows.push([k / 64, len]);
        prev = pt;
      }
      table.push({ p0, g, rows });
      p0 = [g[4], g[5]];
    }
    const at = (L) => {
      for (let s = 0; s < table.length; s++) {
        const rows = table[s].rows;
        if (L <= rows[rows.length - 1][1] + 1e-9 || s === table.length - 1) {
          for (let k = 1; k < rows.length; k++) {
            if (L <= rows[k][1] + 1e-9 || k === rows.length - 1) {
              const [t0, l0] = rows[k - 1];
              const [t1, l1] = rows[k];
              return { s, t: l1 > l0 ? t0 + ((L - l0) / (l1 - l0)) * (t1 - t0) : t1 };
            }
          }
        }
      }
      return { s: table.length - 1, t: 1 };
    };
    let pos = 0;
    let i = 0;
    while (pos < len - 1e-6) {
      const on = pat[i % pat.length];
      const off = pat[(i + 1) % pat.length];
      const a = pos;
      const b = Math.min(len, pos + on);
      if (b - a > 1e-6) {
        const A = at(a);
        const B = at(b);
        const dash = { start: null, segs: [], closed: false };
        for (let s = A.s; s <= B.s; s++) {
          const t0 = s === A.s ? A.t : 0;
          const t1 = s === B.s ? B.t : 1;
          if (t1 - t0 < 1e-6) continue;
          const part = cubicPart(table[s].p0, table[s].g, t0, t1);
          if (!dash.start) dash.start = part.start;
          dash.segs.push(part.seg);
        }
        if (dash.start) out.push(dash);
      }
      pos += on + off;
      i += 2;
    }
  }
  return out;
}

// ── Цвет ───────────────────────────────────────────────────────────────────────────
function rgb(c) {
  if (!c || c === 'none') return null;
  if (c === '#fff' || c === 'white') c = '#ffffff';
  const m = /^#([0-9a-fA-F]{6})$/.exec(c);
  if (!m) throw new Error(`цвет ${c} не поддержан`);
  const n = parseInt(m[1], 16);
  return [((n >> 16) & 255) / 255, ((n >> 8) & 255) / 255, (n & 255) / 255];
}

// ── Кадр SVG → список фигур ────────────────────────────────────────────────────────
// Фигура: { sig, subs (локально), ctm, fill, stroke, opacity, clip, grad }
function drawables(svg, root) {
  const tree = parseSvg(svg);
  const defs = { clip: {}, grad: {} };
  const collectDefs = (node) => {
    for (const c of node.children || []) {
      if (c.tag === 'clipPath') defs.clip[c.attrs.id] = c;
      else if (c.tag === 'linearGradient') defs.grad[c.attrs.id] = c;
      collectDefs(c);
    }
  };
  collectDefs(tree);
  const out = [];
  const inherit = (st, a) => {
    const s = { ...st };
    for (const k of ['fill', 'stroke', 'stroke-width', 'stroke-linecap', 'stroke-linejoin', 'stroke-opacity', 'fill-opacity', 'stroke-dasharray', 'font-size', 'text-anchor']) {
      if (a[k] !== undefined) s[k] = a[k];
    }
    return s;
  };
  const shapeSubs = (node, style) => {
    const a = node.attrs;
    switch (node.tag) {
      case 'path':
        return parsePathD(a.d);
      case 'circle':
        return circleSubs(Number(a.cx || 0), Number(a.cy || 0), Number(a.r));
      case 'rect':
        return rectSubs(Number(a.x || 0), Number(a.y || 0), Number(a.width), Number(a.height), Number(a.rx || 0), Number(a.ry || 0));
      case 'text':
        return textSubs(node, style);
      default:
        return null;
    }
  };
  const walk = (node, ctm, style, opacity, clip) => {
    if (node.tag === 'defs' || node.tag === 'clipPath' || node.tag === 'linearGradient') return;
    const a = node.attrs || {};
    const m = mul(ctm, parseTransform(a.transform));
    const st = inherit(style, a);
    const op = opacity * (a.opacity !== undefined ? Number(a.opacity) : 1);
    let cl = clip;
    if (a['clip-path']) {
      const id = /url\(#([^)]+)\)/.exec(a['clip-path'])[1];
      const cp = defs.clip[id];
      const shape = cp.children.find((c) => c.tag !== '#text');
      cl = { id, subs: shapeSubs(shape, st), ctm: mul(m, parseTransform(shape.attrs.transform)) };
    }
    if (node.tag === 'g' || node.tag === 'svg') {
      for (const c of node.children) walk(c, m, st, op, cl);
      return;
    }
    const subs = shapeSubs(node, st);
    if (!subs) return;
    const fillRaw = st.fill === undefined ? '#000000' : st.fill;
    let fill = null;
    let grad = null;
    if (fillRaw && fillRaw.startsWith('url(')) {
      const g = defs.grad[/url\(#([^)]+)\)/.exec(fillRaw)[1]];
      grad = {
        x1: Number(g.attrs.x1 || 0), y1: Number(g.attrs.y1 || 0), x2: Number(g.attrs.x2 === undefined ? 1 : g.attrs.x2), y2: Number(g.attrs.y2 || 0),
        stops: g.children.filter((c) => c.tag === 'stop').map((s) => ({
          offset: Number(s.attrs.offset),
          color: rgb(s.attrs['stop-color']),
          opacity: s.attrs['stop-opacity'] === undefined ? 1 : Number(s.attrs['stop-opacity']),
        })),
      };
    } else if (fillRaw !== 'none') {
      fill = { color: rgb(fillRaw), opacity: st['fill-opacity'] === undefined ? 1 : Number(st['fill-opacity']) };
    }
    let stroke = null;
    if (st.stroke && st.stroke !== 'none') {
      stroke = {
        color: rgb(st.stroke),
        opacity: st['stroke-opacity'] === undefined ? 1 : Number(st['stroke-opacity']),
        width: Number(st['stroke-width'] === undefined ? 1 : st['stroke-width']),
        cap: { butt: 1, round: 2, square: 3 }[st['stroke-linecap'] || 'butt'],
        join: { miter: 1, round: 2, bevel: 3 }[st['stroke-linejoin'] || 'miter'],
        dash: st['stroke-dasharray'] && st['stroke-dasharray'] !== 'none' ? st['stroke-dasharray'].trim().split(/[\s,]+/).map(Number) : null,
      };
    }
    let geomSubs = subs;
    if (stroke && stroke.dash) {
      const len = pathLength(subs);
      const [on, off] = [stroke.dash[0], stroke.dash.length > 1 ? stroke.dash[1] : stroke.dash[0]];
      // Одна «бегущая» дуга остаётся trim paths, настоящий пунктир — режем на штрихи.
      if (on + off < len * 0.999) {
        geomSubs = dashify(subs, stroke.dash);
        stroke.dash = null;
      }
    }
    const struct = geomSubs.map((s) => `${s.segs.length}${s.closed ? 'z' : ''}`).join('.');
    const paint = (p) => (p ? p.color.map((v) => v.toFixed(3)).join(',') : '-');
    const sig = [
      node.tag === 'text' ? 'text' : 'shape',
      struct,
      grad ? 'grad' : paint(fill),
      // Толщину в подпись не берём: она анимируется дорожкой `w` (тающая волна тоньшает).
      stroke ? `${paint(stroke)}/${stroke.cap}/${stroke.join}/${stroke.dash ? 'dash' : ''}` : '-',
      cl ? cl.id : '',
    ].join('|');
    out.push({ sig, subs: geomSubs, ctm: mul(root, m), fill, grad, stroke, opacity: op, clip: cl ? { ...cl, ctm: mul(root, cl.ctm) } : null });
  };
  walk(tree, I, {}, 1, null);
  return out;
}

// ── Геометрия ──────────────────────────────────────────────────────────────────────
// Все точки подпутей (вершины и управляющие) — для сравнения и подгонки.
function points(subs) {
  const pts = [];
  for (const s of subs) {
    pts.push(s.start);
    for (const g of s.segs) pts.push([g[0], g[1]], [g[2], g[3]], [g[4], g[5]]);
  }
  return pts;
}
const mapSubs = (subs, m) =>
  subs.map((s) => ({
    start: apply(m, s.start),
    closed: s.closed,
    segs: s.segs.map((g) => [...apply(m, [g[0], g[1]]), ...apply(m, [g[2], g[3]]), ...apply(m, [g[4], g[5]])]),
  }));

// Подобие (масштаб + поворот + сдвиг), переводящее точки a в b; null — если не подходит.
function similarity(a, b, tol) {
  const n = a.length;
  let ax = 0;
  let ay = 0;
  let bx = 0;
  let by = 0;
  for (let i = 0; i < n; i++) (ax += a[i][0]), (ay += a[i][1]), (bx += b[i][0]), (by += b[i][1]);
  ax /= n;
  ay /= n;
  bx /= n;
  by /= n;
  let re = 0;
  let im = 0;
  let nn = 0;
  for (let i = 0; i < n; i++) {
    const wx = a[i][0] - ax;
    const wy = a[i][1] - ay;
    const zx = b[i][0] - bx;
    const zy = b[i][1] - by;
    re += zx * wx + zy * wy;
    im += zy * wx - zx * wy;
    nn += wx * wx + wy * wy;
  }
  let p = 1;
  let q = 0;
  if (nn > 1e-12) {
    p = re / nn;
    q = im / nn;
  }
  // z = (p + i·q)(w) + t
  const m = [p, q, -q, p, bx - (p * ax - q * ay), by - (q * ax + p * ay)];
  for (let i = 0; i < n; i++) {
    const [x, y] = apply(m, a[i]);
    if (Math.abs(x - b[i][0]) > tol || Math.abs(y - b[i][1]) > tol) return null;
  }
  return m;
}

// Разложить матрицу без перекоса: T = Tr(p)·Rot(r)·Sc(s)·Tr(−a).
function decompose(m, anchor) {
  const sx = Math.hypot(m[0], m[1]);
  const r = (Math.atan2(m[1], m[0]) * 180) / Math.PI;
  const sy = (m[0] * m[3] - m[1] * m[2]) / sx;
  const p = apply(m, anchor);
  return { p, r, s: [sx * 100, sy * 100] };
}

// Якорь, при котором положение меньше всего меняется (центр поворота или масштаба).
function bestAnchor(ms, fallback) {
  const m0 = ms[0];
  let s00 = 0;
  let s01 = 0;
  let s11 = 0;
  let r0 = 0;
  let r1 = 0;
  for (const m of ms) {
    const b = [m[0] - m0[0], m[1] - m0[1], m[2] - m0[2], m[3] - m0[3]]; // [b00 b10 b01 b11]
    const d = [m[4] - m0[4], m[5] - m0[5]];
    // Bᵀ·B и Bᵀ·d, B = [[b0, b2], [b1, b3]]
    s00 += b[0] * b[0] + b[1] * b[1];
    s01 += b[0] * b[2] + b[1] * b[3];
    s11 += b[2] * b[2] + b[3] * b[3];
    r0 += b[0] * d[0] + b[1] * d[1];
    r1 += b[2] * d[0] + b[3] * d[1];
  }
  const det = s00 * s11 - s01 * s01;
  if (Math.abs(det) < 1e-9) return fallback;
  return [-(s11 * r0 - s01 * r1) / det, -(-s01 * r0 + s00 * r1) / det];
}

// ── Прореживание ключевых кадров (Рамер — Дуглас — Пекер по времени) ───────────────
function reduce(samples, tol) {
  const n = samples.length;
  const keep = new Array(n).fill(false);
  keep[0] = keep[n - 1] = true;
  const dev = (a, b, j) => {
    const k = (j - a) / (b - a);
    let worst = 0;
    const va = samples[a];
    const vb = samples[b];
    const vj = samples[j];
    for (let c = 0; c < vj.length; c++) {
      const t = Array.isArray(tol) ? tol[c] : tol;
      worst = Math.max(worst, Math.abs(vj[c] - (va[c] + (vb[c] - va[c]) * k)) / t);
    }
    return worst;
  };
  const stack = [[0, n - 1]];
  while (stack.length) {
    const [a, b] = stack.pop();
    let worst = 0;
    let at = -1;
    for (let j = a + 1; j < b; j++) {
      const d = dev(a, b, j);
      if (d > worst) (worst = d), (at = j);
    }
    if (worst > 1) {
      keep[at] = true;
      stack.push([a, at], [at, b]);
    }
  }
  return keep.map((k, i) => (k ? i : -1)).filter((i) => i >= 0);
}

const R = (v, d = 2) => {
  const k = 10 ** d;
  const r = Math.round(v * k) / k;
  return Object.is(r, -0) ? 0 : r;
};
const LIN = { i: { x: [1], y: [1] }, o: { x: [0], y: [0] } };

// Свойство Lottie из выборки по кадрам (массивы чисел). wrap — как упаковать значение.
function prop(samples, tol, digits, wrap = (v) => v) {
  const first = samples[0];
  const constant = samples.every((v) => v.every((x, c) => Math.abs(x - first[c]) <= (Array.isArray(tol) ? tol[c] : tol) * 0.5));
  if (constant) return { a: 0, k: wrap(first.map((x) => R(x, digits))) };
  const idx = reduce(samples, tol);
  const k = [];
  for (let j = 0; j < idx.length; j++) {
    const f = idx[j];
    const s = wrap(samples[f].map((x) => R(x, digits)));
    if (j === idx.length - 1) {
      k.push({ t: f, s: Array.isArray(s) ? s : [s] });
    } else {
      const e = wrap(samples[idx[j + 1]].map((x) => R(x, digits)));
      k.push({ t: f, s: Array.isArray(s) ? s : [s], e: Array.isArray(e) ? e : [e], ...LIN });
    }
  }
  return { a: 1, k };
}

// Подпути → форма Lottie (вершины с касательными относительно вершины).
function shapeOf(sub) {
  const v = [sub.start];
  const ii = [[0, 0]];
  const oo = [];
  const segs = sub.segs.slice();
  let closed = sub.closed;
  // Замкнутый путь: последний сегмент, приходящий в начало, — ребро между последней и первой вершиной.
  let closing = null;
  if (closed && segs.length) {
    const last = segs[segs.length - 1];
    if (Math.hypot(last[4] - sub.start[0], last[5] - sub.start[1]) < 1e-6) closing = segs.pop();
  }
  for (const g of segs) {
    const prev = v[v.length - 1];
    oo.push([g[0] - prev[0], g[1] - prev[1]]);
    v.push([g[4], g[5]]);
    ii.push([g[2] - g[4], g[3] - g[5]]);
  }
  if (closing) {
    const prev = v[v.length - 1];
    oo.push([closing[0] - prev[0], closing[1] - prev[1]]);
    ii[0] = [closing[2] - sub.start[0], closing[3] - sub.start[1]];
  } else {
    oo.push([0, 0]);
  }
  return { v, i: ii, o: oo, c: closed };
}
const flatShape = (sh) => [...sh.v.flat(), ...sh.i.flat(), ...sh.o.flat()];
function unflatShape(arr, n, c) {
  const take = (off) => Array.from({ length: n }, (_, k) => [arr[off + 2 * k], arr[off + 2 * k + 1]]);
  return { i: take(2 * n), o: take(4 * n), v: take(0), c };
}

const TOL = { px: 0.25, deg: 0.05, pct: 0.05, op: 0.4, w: 0.05, trim: 0.08 };

// ── Трек фигуры по кадрам → группа Lottie ──────────────────────────────────────────
function trackGroup(frames, kind) {
  // frames[i] — фигура в кадре i или null (в этом кадре её нет).
  const present = frames.map((f, i) => (f ? i : -1)).filter((i) => i >= 0);
  const ref = frames[present[0]];
  // Пропуски заполняем ближайшим кадром, прозрачность — 0.
  const filled = frames.map((f, i) => {
    if (f) return f;
    let best = present[0];
    for (const p of present) if (Math.abs(p - i) < Math.abs(best - i)) best = p;
    return { ...frames[best], opacity: 0 };
  });
  const N = filled.length;
  // Геометрия в кадре i на холсте 512: CTM_i(g_i).
  const geom = (f) => f.ctm;
  const refPts = points(ref.subs);
  // Подходит ли «одна геометрия + анимированное преобразование»?
  const T = [];
  let rigid = true;
  for (const f of filled) {
    const local = similarity(refPts, points(f.subs), 1e-3);
    if (!local) {
      rigid = false;
      break;
    }
    T.push(mul(mul(geom(f), local), inv(geom(ref))));
  }
  const items = [];
  let trs;
  let lscale; // масштаб группы в кадре i — для толщины обводки
  if (rigid) {
    const base = mapSubs(ref.subs, geom(ref));
    const pts = points(base);
    const cx = pts.reduce((s, p) => s + p[0], 0) / pts.length;
    const cy = pts.reduce((s, p) => s + p[1], 0) / pts.length;
    const anchor = bestAnchor(T, [cx, cy]);
    const dec = T.map((m) => decompose(m, anchor));
    for (let i = 1; i < dec.length; i++) {
      while (dec[i].r - dec[i - 1].r > 180) dec[i].r -= 360;
      while (dec[i].r - dec[i - 1].r < -180) dec[i].r += 360;
    }
    for (const sub of base) items.push({ ty: 'sh', ks: { a: 0, k: roundShape(shapeOf(sub)) } });
    trs = {
      ty: 'tr',
      p: prop(dec.map((d) => d.p), TOL.px, 2),
      a: { a: 0, k: [R(anchor[0]), R(anchor[1])] },
      s: prop(dec.map((d) => d.s), TOL.pct, 2),
      r: prop(dec.map((d) => [d.r]), TOL.deg, 2, (v) => v[0]),
      o: { a: 0, k: 100 },
      sk: { a: 0, k: 0 },
      sa: { a: 0, k: 0 },
    };
    lscale = T.map(scaleOf);
  } else {
    // Ключевые кадры геометрии: все кадры должны иметь одинаковое строение.
    const shapes = filled.map((f) => mapSubs(f.subs, geom(f)).map(shapeOf));
    for (let s = 0; s < shapes[0].length; s++) {
      const n = shapes[0][s].v.length;
      const c = shapes[0][s].c;
      const samples = shapes.map((fs) => flatShape(fs[s]));
      const p = prop(samples, TOL.px, 2, (arr) => [unflatShape(arr, n, c)]);
      if (p.a === 0) p.k = p.k[0];
      items.push({ ty: 'sh', ks: p });
    }
    trs = { ty: 'tr', p: { a: 0, k: [0, 0] }, a: { a: 0, k: [0, 0] }, s: { a: 0, k: [100, 100] }, r: { a: 0, k: 0 }, o: { a: 0, k: 100 }, sk: { a: 0, k: 0 }, sa: { a: 0, k: 0 } };
    lscale = filled.map(() => 1);
  }
  if (kind === 'matte') {
    items.push({ ty: 'fl', c: { a: 0, k: [1, 1, 1, 1] }, o: { a: 0, k: 100 }, r: 1, bm: 0 });
  } else {
    // Пунктир, который «бежит» одной дугой (dash + промежуток ≥ длины пути), — trim paths.
    const dash = ref.stroke && ref.stroke.dash;
    if (dash && dash.length === 2) {
      const lens = filled.map((f) => pathLength(mapSubs(f.subs, geom(f))) / scaleOf(geom(f)));
      const single = filled.every((f, i) => f.stroke.dash[0] + f.stroke.dash[1] >= lens[i] * 0.999);
      if (single) {
        items.push({
          ty: 'tm',
          s: { a: 0, k: 0 },
          e: prop(filled.map((f, i) => [Math.min(100, (100 * f.stroke.dash[0]) / lens[i])]), TOL.trim, 2, (v) => v[0]),
          o: { a: 0, k: 0 },
          m: 1,
        });
      }
    }
    if (ref.grad) {
      const g = ref.grad;
      // objectBoundingBox: точки градиента — доли рамки фигуры в её координатах.
      const ends = filled.map((f, i) => {
        const pts = points(f.subs);
        const xs = pts.map((p) => p[0]);
        const ys = pts.map((p) => p[1]);
        const [x0, x1, y0, y1] = [Math.min(...xs), Math.max(...xs), Math.min(...ys), Math.max(...ys)];
        const s = [x0 + g.x1 * (x1 - x0), y0 + g.y1 * (y1 - y0)];
        const e = [x0 + g.x2 * (x1 - x0), y0 + g.y2 * (y1 - y0)];
        const m = rigid ? mul(inv(T[i]), geom(f)) : geom(f);
        return [...apply(m, s), ...apply(m, e)];
      });
      const colors = g.stops.flatMap((s) => [R(s.offset, 3), ...s.color.map((c) => R(c, 3))]);
      const alphas = g.stops.flatMap((s) => [R(s.offset, 3), R(s.opacity, 3)]);
      items.push({
        ty: 'gf',
        o: prop(filled.map((f) => [100 * f.opacity]), TOL.op, 1, (v) => v[0]),
        r: 1,
        bm: 0,
        g: { p: g.stops.length, k: { a: 0, k: [...colors, ...alphas] } },
        s: prop(ends.map((v) => v.slice(0, 2)), TOL.px, 2),
        e: prop(ends.map((v) => v.slice(2)), TOL.px, 2),
        t: 1,
      });
    } else if (ref.fill) {
      items.push({
        ty: 'fl',
        c: { a: 0, k: [...ref.fill.color.map((c) => R(c, 4)), 1] },
        o: prop(filled.map((f) => [100 * f.opacity * f.fill.opacity]), TOL.op, 1, (v) => v[0]),
        r: 1,
        bm: 0,
      });
    }
    if (ref.stroke) {
      // Толщина в системе группы: SVG-толщина × масштаб CTM ÷ масштаб группы.
      const w = filled.map((f, i) => [(f.stroke.width * scaleOf(geom(f))) / lscale[i]]);
      const st = {
        ty: 'st',
        c: { a: 0, k: [...ref.stroke.color.map((c) => R(c, 4)), 1] },
        o: prop(filled.map((f) => [100 * f.opacity * f.stroke.opacity]), TOL.op, 1, (v) => v[0]),
        w: prop(w, TOL.w, 2, (v) => v[0]),
        lc: ref.stroke.cap,
        lj: ref.stroke.join,
        ml: 4,
        bm: 0,
      };
      items.push(st);
    }
  }
  items.push(trs);
  return { ty: 'gr', it: items, np: items.length - 1, cix: 2, bm: 0 };
}

function roundShape(sh) {
  const r = (pts) => pts.map(([x, y]) => [R(x), R(y)]);
  return { i: r(sh.i), o: r(sh.o), v: r(sh.v), c: sh.c };
}

function pathLength(subs) {
  let len = 0;
  for (const s of subs) {
    let [x, y] = s.start;
    for (const g of s.segs) {
      let px = x;
      let py = y;
      for (let k = 1; k <= 24; k++) {
        const t = k / 24;
        const u = 1 - t;
        const nx = u * u * u * x + 3 * u * u * t * g[0] + 3 * u * t * t * g[2] + t * t * t * g[4];
        const ny = u * u * u * y + 3 * u * u * t * g[1] + 3 * u * t * t * g[3] + t * t * t * g[5];
        len += Math.hypot(nx - px, ny - py);
        px = nx;
        py = ny;
      }
      x = g[4];
      y = g[5];
    }
  }
  return len;
}

// ── Значок целиком ─────────────────────────────────────────────────────────────────
function shapeLayer(ind, groups, op, extra = {}) {
  return {
    ddd: 0,
    ind,
    ty: 4,
    nm: `l${ind}`,
    sr: 1,
    ks: { o: { a: 0, k: 100 }, r: { a: 0, k: 0 }, p: { a: 0, k: [0, 0, 0] }, a: { a: 0, k: [0, 0, 0] }, s: { a: 0, k: [100, 100, 100] } },
    ao: 0,
    shapes: groups,
    ip: 0,
    op,
    st: 0,
    bm: 0,
    ...extra,
  };
}

// Telegram требует, чтобы фигуры не выходили за холст. Проверяем каждый кадр по
// рамке точек пути (с учётом толщины обводки); почти прозрачное не считаем.
function outside(name, frames) {
  const bad = [];
  frames.forEach((list, i) => {
    for (const d of list) {
      const vis = d.opacity * (d.fill ? d.fill.opacity : 1) * (d.stroke && !d.fill ? d.stroke.opacity : 1);
      if (vis < 0.03 || d.clip) continue;
      const pts = points(mapSubs(d.subs, d.ctm));
      const half = d.stroke ? (d.stroke.width * scaleOf(d.ctm)) / 2 : 0;
      const xs = pts.map((p) => p[0]);
      const ys = pts.map((p) => p[1]);
      const lo = Math.min(...xs, ...ys) - half;
      const hi = Math.max(...xs, ...ys) + half;
      if (lo < -1 || hi > SIZE + 1) bad.push(`кадр ${i}: ${d.sig.split('|')[0]} ${lo.toFixed(1)}..${hi.toFixed(1)}`);
    }
  });
  if (bad.length) throw new Error(`${name}: выходит за холст — ${bad.slice(0, 3).join('; ')}${bad.length > 3 ? ` (и ещё ${bad.length - 3})` : ''}`);
}

// frameSvgs: SVG-строки кадров 0..N (последний — t = 1, для плавного стыка петли).
function lottieFrom(name, frameSvgs, root, animated) {
  const frames = frameSvgs.map((s) => drawables(s, root));
  outside(name, frames);
  // Сопоставление: подпись + номер среди таких же в кадре.
  const keyed = frames.map((list) => {
    const seen = {};
    return list.map((d) => {
      seen[d.sig] = (seen[d.sig] || 0) + 1;
      return { key: `${d.sig}#${seen[d.sig]}`, d };
    });
  });
  // Общий порядок отрисовки: новые ключи встают после предыдущего известного.
  const order = [];
  for (const list of keyed) {
    let after = -1;
    for (const { key } of list) {
      const at = order.indexOf(key);
      if (at >= 0) after = at;
      else {
        order.splice(after + 1, 0, key);
        after += 1;
      }
    }
  }
  const tracks = order.map((key) => keyed.map((list) => (list.find((x) => x.key === key) || {}).d || null));
  const op = animated ? frameSvgs.length - 1 : STATIC_FRAMES;
  // Подряд идущие фигуры с одной обрезкой — один слой; с обрезкой — пара «матт + слой».
  // Дуга на trim paths — тоже отдельным слоем: так обрезка точно не заденет соседей.
  const runs = [];
  tracks.forEach((tr, n) => {
    const ref = tr.find(Boolean);
    const clip = ref.clip ? ref.clip.id : null;
    const group = ref.stroke && ref.stroke.dash ? `trim${n}` : clip || '';
    const last = runs[runs.length - 1];
    if (last && last.group === group && !group.startsWith('trim')) last.tracks.push(tr);
    else runs.push({ clip, group, tracks: [tr] });
  });
  const layers = [];
  let ind = 1;
  for (const run of runs.reverse()) {
    // В Lottie первая группа — сверху: порядок SVG разворачиваем.
    const groups = run.tracks.slice().reverse().map((tr) => trackGroup(tr, 'shape'));
    if (run.clip) {
      const clipTrack = run.tracks[0].map((d) => (d && d.clip ? { sig: 'clip', subs: d.clip.subs, ctm: d.clip.ctm, opacity: 1 } : null));
      // Кадры без фигуры (блика нет) — берём обрезку из ближайшего кадра.
      layers.push(shapeLayer(ind++, [trackGroup(clipTrack, 'matte')], op, { td: 1 }));
      layers.push(shapeLayer(ind++, groups, op, { tt: 1 }));
    } else {
      layers.push(shapeLayer(ind++, groups, op));
    }
  }
  return { tgs: 1, v: '5.5.2', fr: FPS, ip: 0, op, w: SIZE, h: SIZE, nm: name, ddd: 0, assets: [], layers, markers: [] };
}

const K = SIZE / 24;
const ROOT_COLOR = [K, 0, 0, K, 0, 0];
const KM = SIZE / 25.2;
const ROOT_MONO = [KM, 0, 0, KM, 0.6 * KM, 0.6 * KM];

const svgColor = (body) => `<svg viewBox="0 0 24 24">${body}</svg>`;
const svgMono = (body) =>
  `<svg viewBox="-0.6 -0.6 25.2 25.2"><g fill="none" stroke="#fff" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">${body}</g></svg>`;

function colorLottie(key) {
  const icon = COLOR[key];
  if (!icon.anim) return lottieFrom(key, [svgColor(icon.svg(null))], ROOT_COLOR, false);
  const svgs = [];
  for (let i = 0; i <= FRAMES; i++) svgs.push(svgColor(icon.svg(i / FRAMES)));
  return lottieFrom(key, svgs, ROOT_COLOR, true);
}

function monoLottie(key) {
  return lottieFrom(key, [svgMono(MONO[key])], ROOT_MONO, false);
}

function tgs(doc) {
  return zlib.gzipSync(Buffer.from(JSON.stringify(doc)), { level: 9 });
}

function main() {
  const OUT = path.resolve(process.argv[2] || 'assets/emoji_tgs');
  const only = process.argv[3] ? process.argv[3].split(',') : null;
  const report = [];
  for (const [kind, keys, make] of [
    ['mono', Object.keys(MONO), monoLottie],
    ['color', Object.keys(COLOR), colorLottie],
  ]) {
    fs.mkdirSync(path.join(OUT, kind), { recursive: true });
    for (const key of keys) {
      if (only && !only.includes(key)) continue;
      const doc = make(key);
      const buf = tgs(doc);
      if (buf.length > LIMIT) throw new Error(`${kind}/${key}.tgs: ${buf.length} байт — больше 64 КБ`);
      fs.writeFileSync(path.join(OUT, kind, `${key}.tgs`), buf);
      if (process.env.EXCH_JSON) fs.writeFileSync(path.join(OUT, kind, `${key}.json`), JSON.stringify(doc));
      report.push({ kind, key, bytes: buf.length, animated: doc.op !== STATIC_FRAMES });
    }
  }
  const sum = (k) => report.filter((r) => r.kind === k).length;
  const big = report.slice().sort((a, b) => b.bytes - a.bytes).slice(0, 5);
  console.log(`моно: ${sum('mono')}, цвет: ${sum('color')}; крупнейшие: ${big.map((r) => `${r.kind}/${r.key} ${r.bytes}`).join(', ')}`);
}

module.exports = { colorLottie, monoLottie, lottieFrom, drawables, parsePathD, tgs, FRAMES, STATIC_FRAMES };

if (require.main === module) main();
