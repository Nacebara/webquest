const { chromium } = require('playwright');
(async () => {
  const b = await chromium.launch();
  const p = await b.newPage({ viewport: { width: 800, height: 800 }, deviceScaleFactor: 1 });
  await p.goto('file://' + __dirname + '/avatar.html');
  await p.waitForFunction(() => document.title === 'ready');
  const N = 90;
  for (let i = 0; i < N; i++) {
    await p.evaluate((t) => window.frame(t), i / N);
    await p.locator('#a').screenshot({ path: `${process.argv[2] || __dirname}/f${String(i).padStart(3, '0')}.png` });
  }
  await b.close();
})();
