// Serves this directory and runs web/index.html in a browser through
// Playwright, then prints the page's report (window.benchResult).
//
//   node web.mjs [--browser webkit|chromium] [--repeats N]
//
// WebKit is the engine of today's in-app kernel (WKWebView); on a Mac it
// is the closest to the apps, short of the apps themselves.
import { createReadStream, statSync } from "node:fs";
import { createServer } from "node:http";
import { extname, join, normalize } from "node:path";
import { fileURLToPath } from "node:url";
import * as playwright from "playwright";

const args = process.argv.slice(2);
const opt = (name, fallback) => (args.includes(name) ? args[args.indexOf(name) + 1] : fallback);
const browserName = opt("--browser", "webkit");
const repeats = opt("--repeats", "5");

const root = fileURLToPath(new URL(".", import.meta.url));
const types = { ".html": "text/html", ".js": "text/javascript", ".json": "application/json", ".map": "application/json" };
const server = createServer((req, res) => {
  const path = normalize(join(root, decodeURIComponent(new URL(req.url, "http://x").pathname)));
  if (!path.startsWith(root)) return res.writeHead(403).end();
  try {
    if (!statSync(path).isFile()) throw new Error("not a file");
  } catch {
    return res.writeHead(404).end();
  }
  res.writeHead(200, { "content-type": types[extname(path)] ?? "application/octet-stream" });
  createReadStream(path).pipe(res);
});
await new Promise((r) => server.listen(0, "127.0.0.1", r));
const { port } = server.address();

const browser = await playwright[browserName].launch();
try {
  const page = await browser.newPage();
  page.on("pageerror", (e) => console.error("page error:", e.message));
  await page.goto(`http://127.0.0.1:${port}/web/index.html?repeats=${repeats}`);
  await page.waitForFunction(() => window.benchResult, null, { timeout: 300_000 });
  const report = await page.evaluate(() => window.benchResult);
  report.engine = `worker (${browserName} ${browser.version()})`;
  console.log(JSON.stringify(report, null, 2));
} finally {
  await browser.close();
  server.close();
}
