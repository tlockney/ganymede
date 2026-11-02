// jupyterlab-lsp in headless Chromium against the server under test: a
// TypeScript file and a Python notebook, each with a type error, must show
// a diagnostic from the language server (typescript-language-server and
// pyright), reached through GET /lsp/status and WS /lsp/ws/{server}. The
// notebook goes through the virtual documents path.
//
// Usage: node lsp.mjs <base-url> <token> <out.json>
// The server needs jupyterlab-lsp's extension and both language servers
// configured: for Ganymede, `[language_servers.*]` in ganymede.toml; for
// jupyter_server, jupyter-lsp (the oracle).
import { chromium } from 'playwright';
import fs from 'node:fs';

const [base, token, out] = process.argv.slice(2);
if (!out) {
  console.error('usage: node lsp.mjs <base-url> <token> <out.json>');
  process.exit(2);
}
const stamp = Date.now();
const auth = { Authorization: `token ${token}`, 'Content-Type': 'application/json' };
const put = async (name, model) => {
  const r = await fetch(`${base}/api/contents/${name}`, { method: 'PUT', headers: auth, body: JSON.stringify(model) });
  if (!r.ok) throw new Error(`creating ${name}: ${r.status}`);
};

const tsName = `lsp-${stamp}.ts`;
await put(tsName, { type: 'file', format: 'text', content: 'const n: number = "not a number";\nexport { n };\n' });
const nbName = `lsp-${stamp}.ipynb`;
await put(nbName, {
  type: 'notebook',
  format: 'json',
  content: {
    cells: [{ cell_type: 'code', execution_count: null, id: 'c1', metadata: {}, outputs: [], source: 'print(undefined_name)' }],
    metadata: {
      kernelspec: { name: 'python3', display_name: 'Python 3', language: 'python' },
      language_info: { name: 'python', mimetype: 'text/x-python', file_extension: '.py' },
    },
    nbformat: 4,
    nbformat_minor: 5,
  },
});

const status = await (await fetch(`${base}/lsp/status`, { headers: auth })).json().catch(() => null);
const result = { status: status && { version: status.version, specs: Object.keys(status.specs ?? {}).sort() } };
const log = [];
const consoleErrors = [];
const browser = await chromium.launch();
const origin = new URL(base).origin;
const clean = u => { const x = new URL(u); x.searchParams.delete('token'); return x.pathname; };

// Opens `name` in a fresh workspace and waits for a lint mark in its editor.
async function diagnostics(key, name, selector) {
  const page = await browser.newPage();
  page.on('websocket', ws => { if (clean(ws.url()).startsWith('/lsp/')) log.push({ doc: key, ws: clean(ws.url()) }); });
  page.on('console', m => { if (m.type() === 'error') consoleErrors.push(`${key}: ${m.text().slice(0, 300)}`); });
  page.on('pageerror', e => consoleErrors.push(`${key}: pageerror: ${String(e).slice(0, 300)}`));
  try {
    await page.goto(`${base}/lab/workspaces/${key}-${stamp}/tree/${name}?reset&token=${token}`);
    await page.waitForSelector(selector, { timeout: 60000 });
    // Language servers start on first use; pyright can take a while.
    const mark = await page.waitForSelector('.cm-lintRange-error, .cm-lintRange-warning', { timeout: 90000 });
    result[key] = { diagnostic: (await mark.textContent()).trim() };
  } catch (e) {
    result[key] = { error: String(e).slice(0, 500) };
  }
  await page.screenshot({ path: out.replace(/\.json$/, `-${key}.png`) }).catch(() => {});
  await page.close();
}

await diagnostics('typescript', tsName, '.jp-FileEditor .cm-content');
await diagnostics('notebook', nbName, '.jp-Notebook .jp-Cell .cm-content');
await browser.close();
for (const n of [tsName, nbName]) await fetch(`${base}/api/contents/${n}`, { method: 'DELETE', headers: auth });

fs.writeFileSync(out, JSON.stringify({ result, consoleErrors, log }, null, 1));
console.log(JSON.stringify(result));
// Under jupyter-lsp the page throws nothing; with the core language-server
// plugins left on next to jupyterlab-lsp's, it throws on every edit.
const pageErrors = consoleErrors.filter(e => e.includes('pageerror:'));
const ok = result.status?.version === 2 && result.typescript?.diagnostic === 'n' &&
  result.notebook?.diagnostic === 'undefined_name' && pageErrors.length === 0;
if (pageErrors.length) console.error(pageErrors.slice(0, 3).join('\n'));
process.exit(ok ? 0 : 1);
