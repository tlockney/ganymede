// Notebook trust in JupyterLab, served by the server under test, in
// headless Chromium: runs a cell whose HTML and JavaScript outputs set
// markers on the page, saves, and reopens the notebook in a fresh page. A
// notebook saved with outputs from the session must come back trusted, so
// the scripts run again; a copy saved without the cells' `trusted` flags
// must not.
//
// Usage: node trust.mjs <base-url> <token> <kernel-name> <out.json>
// Env: CODE (cell source; must display HTML whose <script> sets
//      document.body.dataset.probeHtml and JavaScript that sets
//      document.body.dataset.probeJs), WS_TOKEN=1 as in jupyterlab.mjs.
import { chromium } from 'playwright';
import fs from 'node:fs';

const [base, token, kernel, out] = process.argv.slice(2);
const code = process.env.CODE ??
  "from IPython.display import HTML, Javascript, display\n" +
  "display(HTML('<b class=\"probe\">bold</b><script>document.body.dataset.probeHtml = \"ran\"</script>'))\n" +
  "display(Javascript('document.body.dataset.probeJs = \"ran\"'))";
const stamp = Date.now();
const name = `trust-${stamp}.ipynb`;
const copy = `trust-${stamp}-untrusted.ipynb`;
const auth = { Authorization: `token ${token}`, 'Content-Type': 'application/json' };
const api = (path, init = {}) => fetch(`${base}/api/contents/${path}`, { headers: auth, ...init });

const notebook = {
  cells: [{ cell_type: 'code', execution_count: null, id: 'c1', metadata: {}, outputs: [], source: '' }],
  metadata: { kernelspec: { name: kernel, display_name: kernel, language: '' } },
  nbformat: 4, nbformat_minor: 5,
};
const put = await api(name, { method: 'PUT', body: JSON.stringify({ type: 'notebook', content: notebook }) });
if (!put.ok) throw new Error(`creating ${name}: ${put.status}`);

const consoleErrors = [];
const browser = await chromium.launch();
async function open(path, workspace) {
  const page = await browser.newPage();
  if (process.env.WS_TOKEN) {
    await page.addInitScript(t => {
      const WS = window.WebSocket;
      window.WebSocket = function (url, protocols) {
        const u = new URL(url, location.href);
        u.searchParams.set('token', t);
        return new WS(u.toString(), protocols);
      };
      Object.assign(window.WebSocket, WS);
      window.WebSocket.prototype = WS.prototype;
    }, token);
  }
  page.on('console', m => { if (m.type() === 'error') consoleErrors.push(m.text().slice(0, 300)); });
  page.on('pageerror', e => consoleErrors.push('pageerror: ' + String(e).slice(0, 300)));
  await page.goto(`${base}/lab/workspaces/${workspace}/tree/${path}?token=${token}`);
  await page.waitForSelector('.jp-Notebook .jp-Cell', { timeout: 60000 });
  return page;
}
const probes = page => page.evaluate(() => ({
  html: document.body.dataset.probeHtml ?? null,
  js: document.body.dataset.probeJs ?? null,
  rendered: !!document.querySelector('.jp-OutputArea-output b.probe'),
}));

const result = {};
try {
  const page = await open(name, `trust-run-${stamp}`);
  await page.waitForSelector('.jp-StatusBar-TextItem:has-text("| Idle")', { timeout: 60000 });
  await page.click('.jp-Notebook .jp-Cell .cm-content');
  await page.keyboard.insertText(code);
  await page.keyboard.press('Shift+Enter');
  await page.waitForSelector('.jp-OutputArea-output b.probe', { timeout: 60000 });
  await page.waitForTimeout(1000);
  result.live = await probes(page);
  await page.keyboard.press('Control+s');
  await page.waitForTimeout(2000);
  await page.close();

  const saved = await (await api(name)).json();
  result.servedTrusted = saved.content.cells[0].metadata.trusted ?? null;

  // The notebook, changed so its signature differs, saved by a client
  // that does not vouch for its cells.
  const content = saved.content;
  content.metadata.edited_elsewhere = true;
  for (const cell of content.cells) delete cell.metadata.trusted;
  await api(copy, { method: 'PUT', body: JSON.stringify({ type: 'notebook', content }) });
  result.copyServedTrusted = (await (await api(copy)).json()).content.cells[0].metadata.trusted ?? null;

  const reopened = await open(name, `trust-reopen-${stamp}`);
  await reopened.waitForSelector('.jp-OutputArea-output', { timeout: 60000 });
  await reopened.waitForTimeout(2000);
  result.reopened = await probes(reopened);
  await reopened.screenshot({ path: out.replace(/\.json$/, '.png') }).catch(() => {});
  await reopened.close();

  const untrusted = await open(copy, `trust-copy-${stamp}`);
  await untrusted.waitForSelector('.jp-OutputArea-output', { timeout: 60000 });
  await untrusted.waitForTimeout(2000);
  result.untrustedCopy = await probes(untrusted);
  await untrusted.close();
} catch (e) {
  result.error = String(e).slice(0, 500);
}
await browser.close();
for (const p of [name, copy]) await api(p, { method: 'DELETE' });
fs.writeFileSync(out, JSON.stringify({ result, consoleErrors }, null, 1));
console.log(JSON.stringify(result));
const ok = result.servedTrusted === true &&
  result.reopened?.html === 'ran' && result.reopened?.js === 'ran' &&
  result.copyServedTrusted === false && result.untrustedCopy?.js === null && result.untrustedCopy?.html === null;
process.exit(ok ? 0 : 1);
