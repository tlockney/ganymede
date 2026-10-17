// Drives JupyterLab, served by the server under test, in headless Chromium:
// creates a notebook for a kernelspec over REST, opens it, runs one cell,
// saves, and records every request the page makes.
//
// Usage: node jupyterlab.mjs <base-url> <token> <kernel-name> <out.json>
// Env: CODE (cell source), EXPECT (text the output must contain).
// The page is opened with `?token=`, which the server turns into a login
// cookie; the websockets authenticate with that cookie, as in a browser.
import { chromium } from 'playwright';
import fs from 'node:fs';

const [base, token, kernel, out] = process.argv.slice(2);
const code = process.env.CODE ?? 'print("hi from " + str(6*7))';
const expect = process.env.EXPECT ?? 'hi from 42';
const name = `validate-${Date.now()}.ipynb`;
const auth = { Authorization: `token ${token}`, 'Content-Type': 'application/json' };

const notebook = {
  cells: [{ cell_type: 'code', execution_count: null, id: 'c1', metadata: {}, outputs: [], source: '' }],
  metadata: { kernelspec: { name: kernel, display_name: kernel, language: '' } },
  nbformat: 4, nbformat_minor: 5,
};
const put = await fetch(`${base}/api/contents/${name}`, {
  method: 'PUT', headers: auth, body: JSON.stringify({ type: 'notebook', format: 'json', content: notebook }),
});
if (!put.ok) throw new Error(`creating ${name}: ${put.status}`);

const log = [];
const consoleErrors = [];
const browser = await chromium.launch();
const page = await browser.newPage();
const origin = new URL(base).origin;
const clean = u => { const x = new URL(u); x.searchParams.delete('token'); return x.pathname + x.search; };
page.on('response', r => { if (r.url().startsWith(origin)) log.push({ method: r.request().method(), path: clean(r.url()), status: r.status() }); });
page.on('websocket', ws => {
  log.push({ method: 'WS', path: clean(ws.url()), status: 'open' });
  ws.on('close', () => log.push({ method: 'WS', path: clean(ws.url()), status: 'close' }));
});
page.on('console', m => { if (m.type() === 'error') consoleErrors.push(m.text().slice(0, 300)); });
page.on('pageerror', e => consoleErrors.push('pageerror: ' + String(e).slice(0, 300)));

const result = {};
try {
  // A fresh workspace, so no tabs from earlier sessions are restored.
  await page.goto(`${base}/lab/workspaces/${name.replace('.ipynb', '')}/tree/${name}?token=${token}`);
  await page.waitForSelector('.jp-Notebook .jp-Cell', { timeout: 60000 });
  result.opened = true;
  // The kernel is ready once the status bar says Idle.
  await page.waitForSelector('.jp-StatusBar-TextItem:has-text("| Idle")', { timeout: 60000 });
  result.kernelIdle = true;
  await page.click('.jp-Notebook .jp-Cell .cm-content');
  await page.keyboard.type(code);
  await page.keyboard.press('Shift+Enter');
  const output = await page.waitForSelector('.jp-OutputArea-output', { timeout: 60000 });
  result.output = (await output.textContent()).trim();
  await page.keyboard.press('ControlOrMeta+s');
  await page.waitForTimeout(2000);
  const saved = await (await fetch(`${base}/api/contents/${name}`, { headers: auth })).json();
  result.saved = JSON.stringify(saved.content.cells[0].outputs).includes(expect);
} catch (e) {
  result.error = String(e).slice(0, 500);
}
await page.screenshot({ path: out.replace(/\.json$/, '.png') }).catch(() => {});
await browser.close();
await fetch(`${base}/api/contents/${name}`, { method: 'DELETE', headers: auth });
fs.writeFileSync(out, JSON.stringify({ result, consoleErrors, log }, null, 1));
console.log(JSON.stringify(result));
process.exit(result.output?.includes(expect) && result.saved ? 0 : 1);
