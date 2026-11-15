const { chromium } = require('playwright');
(async () => {
  const D = __dirname, b = await chromium.launch();
  const p = await b.newPage({ viewport: { width: 1400, height: 1500 }, deviceScaleFactor: 2 });
  await p.goto('file://' + D + '/logo3.html'); await p.waitForFunction(() => document.title === 'ready');
  await p.locator('#g').screenshot({ path: D + '/sheet.png' });
  const q = await b.newPage({ viewport: { width: 640, height: 640 }, deviceScaleFactor: 1 });
  await q.goto('file://' + D + '/logo3.html'); await q.waitForFunction(() => document.title === 'ready');
  const n = await q.evaluate(() => window.N);
  for (let i = 0; i < n; i++) { await q.evaluate((i) => window.SOLO(i), i); await q.locator('#solo').screenshot({ path: `${D}/m${i + 1}.png` }); }
  await b.close(); console.log('ok', n);
})();
