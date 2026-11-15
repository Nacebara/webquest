const { chromium } = require('playwright');
(async () => { const D = __dirname, b = await chromium.launch();
 const p = await b.newPage({ viewport: { width: 1360, height: 1200 }, deviceScaleFactor: 2 });
 await p.goto('file://' + D + '/logo.html'); await p.waitForFunction(() => document.title === 'ready');
 await p.locator('#root').screenshot({ path: D + '/sheet.png' });
 const s = await b.newPage({ viewport: { width: 640, height: 640 } });
 await s.goto('file://' + D + '/logo.html'); await s.waitForFunction(() => document.title === 'ready');
 for (let i = 0; i < 3; i++) { await s.evaluate((i) => window.SOLO(i), i); await s.locator('#solo').screenshot({ path: `${D}/f${i + 1}.png` }); }
 await b.close(); console.log('ok'); })();
