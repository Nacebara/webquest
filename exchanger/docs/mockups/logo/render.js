const { chromium } = require('playwright');
(async () => {
  const D = __dirname;
  const b = await chromium.launch();
  const p = await b.newPage({ viewport: { width: 700, height: 700 } });
  await p.goto('file://' + D + '/logo.html');
  await p.waitForFunction(() => document.title === 'ready');
  await p.waitForTimeout(300);
  const vs = await p.evaluate(() => window.VARIANTS);
  for (const v of vs) {
    await p.locator('#' + v.id).screenshot({ path: `${D}/${v.id}.png` });
  }
  console.log(vs.map(v => v.id).join(' '));
  await b.close();
})();
