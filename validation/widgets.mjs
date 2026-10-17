// Drives JupyterLab in headless Chromium through an anywidget widget:
// creates a notebook whose first cell makes cdp-kernel's counter widget
// (docs/widgets.md in cdp-kernel), runs it, clicks the button, and asks
// the kernel for the count in a second cell. The frontend needs the
// anywidget and jupyterlab_widgets extensions (`ganymede lab install`).
// With RELOAD=1 it then saves the notebook, opens it in a fresh page, and
// checks that the widget renders again with the clicked count, restored
// from the running kernel. Exits non-zero when a check fails.
//
// Usage: node widgets.mjs <base-url> <token> <kernel-name> <out.json>
import { chromium } from 'playwright';
import fs from 'node:fs';

const [base, token, kernel, out] = process.argv.slice(2);
const name = `widgets-${Date.now()}.ipynb`;
const auth = { Authorization: `token ${token}`, 'Content-Type': 'application/json' };

const counter = `const counter = jupyter.widget({
  esm: \`export default {
    render({ model, el }) {
      const button = document.createElement("button");
      const update = () => {
        button.textContent = "count is " + model.get("count");
      };
      button.onclick = () => {
        model.set("count", model.get("count") + 1);
        model.save_changes();
      };
      model.on("change:count", update);
      update();
      el.append(button);
    },
  };\`,
  state: { count: 0 },
});
counter;`;
const cell = (id, source) => ({ cell_type: 'code', execution_count: null, id, metadata: {}, outputs: [], source });
const notebook = {
  cells: [cell('c1', counter), cell('c2', 'counter.get("count")')],
  metadata: { kernelspec: { name: kernel, display_name: kernel, language: '' } },
  nbformat: 4, nbformat_minor: 5,
};
const put = await fetch(`${base}/api/contents/${name}`, {
  method: 'PUT', headers: auth, body: JSON.stringify({ type: 'notebook', format: 'json', content: notebook }),
});
if (!put.ok) throw new Error(`creating ${name}: ${put.status}`);

const consoleErrors = [];
const extensions = new Set();
const browser = await chromium.launch();
const page = await browser.newPage();
const failed = [];
page.on('response', r => {
  if (r.status() >= 400) failed.push(`${r.status()} ${new URL(r.url()).pathname}`);
  const m = new URL(r.url()).pathname.match(/\/lab\/extensions\/((?:@[^/]+\/)?[^/]+)\//);
  if (m && r.ok()) extensions.add(m[1]);
});
page.on('console', m => { if (m.type() === 'error') consoleErrors.push(m.text().slice(0, 300)); });
page.on('pageerror', e => consoleErrors.push('pageerror: ' + String(e).slice(0, 300)));

const result = {};
try {
  await page.goto(`${base}/lab/workspaces/${name.replace('.ipynb', '')}/tree/${name}?token=${token}`);
  await page.waitForSelector('.jp-Notebook .jp-Cell', { timeout: 60000 });
  await page.waitForSelector('.jp-StatusBar-TextItem:has-text("| Idle")', { timeout: 60000 });
  result.kernelIdle = true;
  const cells = page.locator('.jp-Notebook .jp-CodeCell');
  await cells.nth(0).locator('.cm-content').click();
  await page.keyboard.press('Shift+Enter');
  const button = cells.nth(0).locator('.jp-OutputArea-output button');
  await button.waitFor({ timeout: 60000 });
  result.rendered = (await button.textContent()).trim();
  await page.screenshot({ path: out.replace(/\.json$/, '-rendered.png') });
  for (let i = 0; i < 3; i++) await button.click();
  await page.waitForFunction(
    el => el.textContent.includes('count is 3'), await button.elementHandle(), { timeout: 10000 });
  result.afterClicks = (await button.textContent()).trim();
  await cells.nth(1).locator('.cm-content').click();
  await page.keyboard.press('Shift+Enter');
  const reply = cells.nth(1).locator('.jp-OutputArea-output');
  await reply.waitFor({ timeout: 60000 });
  result.kernelCount = (await reply.textContent()).trim();
  if (process.env.RELOAD) {
    await page.keyboard.press('Control+s');
    await page.waitForFunction(
      () => !document.querySelector('.jp-mod-dirty'), null, { timeout: 10000 });
    const saved = await (await fetch(`${base}/api/contents/${name}`, { headers: auth })).json();
    result.savedOutputs = saved.content.cells.map(c => c.outputs.flatMap(o => Object.keys(o.data ?? {})));
    const again = await browser.newPage();
    again.on('pageerror', e => consoleErrors.push('reload pageerror: ' + String(e).slice(0, 300)));
    await again.goto(`${base}/lab/workspaces/${name.replace('.ipynb', '')}-reload/tree/${name}?token=${token}`);
    const restored = again.locator('.jp-Notebook .jp-CodeCell').nth(0).locator('.jp-OutputArea-output button');
    await restored.waitFor({ timeout: 60000 });
    await again.waitForFunction(
      el => el.textContent.includes('count is 3'), await restored.elementHandle(), { timeout: 10000 });
    result.reloaded = (await restored.textContent()).trim();
    await again.screenshot({ path: out.replace(/\.json$/, '-reloaded.png') });
    await again.close();
  }
} catch (e) {
  result.error = String(e).slice(0, 500);
}
result.extensions = [...extensions].sort();
await page.screenshot({ path: out.replace(/\.json$/, '.png') }).catch(() => {});
await browser.close();
await fetch(`${base}/api/contents/${name}`, { method: 'DELETE', headers: auth });
fs.writeFileSync(out, JSON.stringify({ result, failed, consoleErrors }, null, 1));
console.log(JSON.stringify(result));
const ok = !result.error && result.afterClicks === 'count is 3' && result.kernelCount === '3' &&
  (!process.env.RELOAD || result.reloaded === 'count is 3');
process.exit(ok ? 0 : 1);
