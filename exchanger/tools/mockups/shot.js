// Снимок первой группы вкладки «Пульт» (её вставляет proposal.py). Отказ — если на странице
// ошибка, нет lottie или хоть один значок не стал премиум-эмодзи: такая картинка владельцу не уходит.
const { chromium } = require('playwright');
const [page_path, out] = process.argv.slice(2);
(async () => {
  const b = await chromium.launch();
  const p = await b.newPage({ viewport: { width: 1400, height: 5200 }, deviceScaleFactor: 2 });
  const errs = [];
  p.on('pageerror', (e) => errs.push(String(e)));
  p.on('requestfailed', (r) => errs.push('request ' + r.url()));
  await p.goto('file://' + page_path + '#admin');
  await p.evaluate(() => { for (const e of document.querySelectorAll('*')) { const cs = getComputedStyle(e); if (cs.position === 'sticky' || cs.position === 'fixed') e.style.position = 'static'; } });
  await p.evaluate(() => document.querySelector('#g0').scrollIntoView());
  let left = -1;
  for (let i = 0; i < 40; i++) {
    await p.waitForTimeout(500);
    left = await p.evaluate(() => [...document.querySelectorAll('#g0 .ce')].filter((el) => !el._a).length);
    if (left === 0) break;
  }
  const total = await p.evaluate(() => document.querySelectorAll('#g0 .ce').length);
  const lottie = await p.evaluate(() => typeof window.lottie);
  const el = await p.$('#g0');
  await el.screenshot({ path: out });
  await b.close();
  console.log(`значков ${total}, не отрисовано ${left}, lottie ${lottie}, ошибок ${errs.length}`);
  if (left !== 0 || lottie !== 'object' || errs.length) {
    console.error(errs.join('\n'));
    process.exit(1);
  }
})();
