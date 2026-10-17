// Opens the URL `ganymede lab` printed, as a browser opened on it would,
// and checks that it lands logged in on JupyterLab: the token is traded
// for the login cookie, the address bar ends at /lab without it, the
// shell renders, and the cookie alone lists the expected kernelspec.
//
// Usage: node login.mjs <printed-url> <kernel-name> <out.png>
import { chromium } from 'playwright';

const [url, kernel, out] = process.argv.slice(2);
const browser = await chromium.launch();
const page = await browser.newPage();
const result = {};
try {
  await page.goto(url);
  await page.waitForSelector('#jp-main-dock-panel', { timeout: 60000 });
  const landed = new URL(page.url());
  result.landed = landed.pathname + landed.search;
  result.cookies = (await page.context().cookies()).map(c => c.name).sort();
  const specs = await page.request.get(new URL('/api/kernelspecs', landed).href);
  result.kernelspecsStatus = specs.status();
  result.kernelspecs = specs.ok() ? Object.keys((await specs.json()).kernelspecs).sort() : [];
} catch (e) {
  result.error = String(e).slice(0, 500);
}
await page.screenshot({ path: out }).catch(() => {});
await browser.close();
console.log(JSON.stringify(result));
const ok = !result.error && result.landed === '/lab' &&
  result.cookies.some(c => c.startsWith('username-')) && result.kernelspecs.includes(kernel);
process.exit(ok ? 0 : 1);
