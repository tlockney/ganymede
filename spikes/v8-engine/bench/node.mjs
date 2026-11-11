// Runs workloads/manifest.json's workloads on Node, each in a fresh
// worker_threads Worker (a new isolate, as a kernel start would be), and
// prints a report shaped like v8-bench's.
//
//   node [--jitless] node.mjs [--repeats N]
//
// Node is the reference: the engine cdp-kernel runs on the Mac.
import { readFileSync } from "node:fs";
import { Worker } from "node:worker_threads";

const here = new URL(".", import.meta.url);
const args = process.argv.slice(2);
const repeats = args.includes("--repeats") ? Number(args[args.indexOf("--repeats") + 1]) : 5;
const read = (rel) => readFileSync(new URL(rel, here), "utf8");
const manifest = JSON.parse(read("workloads/manifest.json"));

// The Worker's side: load the sources, then answer "run" messages.
const workerCode = `
const { parentPort } = require("node:worker_threads");
const vm = require("node:vm");
const v8 = require("node:v8");
parentPort.postMessage({ op: "ready" });
parentPort.on("message", (m) => {
  try {
    if (m.op === "load") {
      for (const src of m.sources) vm.runInThisContext(src);
      parentPort.postMessage({ op: "loaded" });
    } else if (m.op === "run") {
      parentPort.postMessage({ op: "result", result: globalThis.workload.run() });
    } else if (m.op === "heap") {
      const s = v8.getHeapStatistics();
      parentPort.postMessage({ op: "heap", heap: {
        used_bytes: s.used_heap_size, total_bytes: s.total_heap_size,
        physical_bytes: s.total_physical_size, external_bytes: s.external_memory,
        peak_malloced_bytes: s.peak_malloced_memory } });
    }
  } catch (e) {
    parentPort.postMessage({ op: "error", error: String(e && e.stack || e) });
  }
});
`;

function next(worker) {
  return new Promise((resolve, reject) => {
    worker.once("message", (m) => (m.op === "error" ? reject(new Error(m.error)) : resolve(m)));
    worker.once("error", reject);
  });
}

const median = (xs) => {
  if (!xs.length) return null;
  const s = [...xs].sort((a, b) => a - b);
  const n = s.length;
  return n % 2 ? s[(n - 1) / 2] : (s[n / 2 - 1] + s[n / 2]) / 2;
};

async function runWorkload({ name, sources }) {
  const report = { name, error: null };
  const texts = sources.map(read);
  let t = performance.now();
  const worker = new Worker(workerCode, { eval: true });
  try {
    await next(worker);
    report.start_ms = performance.now() - t;
    t = performance.now();
    worker.postMessage({ op: "load", sources: texts });
    await next(worker);
    report.load_ms = performance.now() - t;
    const times = [];
    for (let i = 0; i < repeats; i++) {
      t = performance.now();
      worker.postMessage({ op: "run" });
      const { result } = await next(worker);
      times.push(performance.now() - t);
      report.result_bytes = Buffer.byteLength(result);
      report.result_preview = result.slice(0, 80);
    }
    report.first_ms = times[0];
    report.steady_ms = median(times.slice(1));
    report.cold_ms = report.start_ms + report.load_ms + report.first_ms;
    worker.postMessage({ op: "heap" });
    report.heap = (await next(worker)).heap;
  } catch (e) {
    report.error = String(e.message ?? e);
  } finally {
    await worker.terminate();
  }
  return report;
}

const workloads = [];
for (const w of manifest.workloads) workloads.push(await runWorkload(w));
console.log(
  JSON.stringify(
    {
      engine: "node",
      version: process.versions.node + " (V8 " + process.versions.v8 + ")",
      jitless: process.execArgv.includes("--jitless"),
      init_ms: 0,
      workloads,
      max_rss_bytes: process.resourceUsage().maxRSS * 1024,
    },
    null,
    2,
  ),
);
