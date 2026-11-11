// Runs workloads/manifest.json's workloads in this browser's Workers, each
// in a fresh Worker, and cold-starts today's in-app kernel (cdp-kernel's
// browser kernel, from dist/kernel) to its first result. Sets
// window.benchResult to a report shaped like v8-bench's, and shows it.
//
// ?repeats=N sets the calls per workload (default 5).

const params = new URLSearchParams(location.search);
const repeats = Number(params.get("repeats") ?? 5);
const base = new URL("../", location.href);
const text = async (rel) => (await fetch(new URL(rel, base))).text();

const median = (xs) => {
  if (!xs.length) return null;
  const s = [...xs].sort((a, b) => a - b);
  const n = s.length;
  return n % 2 ? s[(n - 1) / 2] : (s[n / 2 - 1] + s[n / 2]) / 2;
};

function next(worker) {
  return new Promise((resolve, reject) => {
    worker.onmessage = (e) => (e.data.op === "error" ? reject(new Error(e.data.error)) : resolve(e.data));
    worker.onerror = (e) => reject(new Error(e.message || "worker error"));
  });
}

async function runWorkload({ name, sources }) {
  const report = { name, error: null, heap: null };
  const texts = await Promise.all(sources.map(text));
  let t = performance.now();
  const worker = new Worker(new URL("worker.js", location.href));
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
      report.result_bytes = new TextEncoder().encode(result).length;
      report.result_preview = result.slice(0, 80);
    }
    report.first_ms = times[0];
    report.steady_ms = median(times.slice(1));
    report.cold_ms = report.start_ms + report.load_ms + report.first_ms;
  } catch (e) {
    report.error = String(e.message ?? e);
  } finally {
    worker.terminate();
  }
  return report;
}

/** A Jupyter message from the frontend. */
function request(msgType, content) {
  return {
    header: {
      msg_id: crypto.randomUUID(),
      msg_type: msgType,
      session: "bench",
      username: "bench",
      date: new Date().toISOString(),
      version: "5.3",
    },
    parent_header: {},
    metadata: {},
    content,
  };
}

/**
 * Today's in-app kernel: import the kernel module, start it, and run
 * `1 + 1` to its execute_result. start_ms is import and start to
 * kernel_info_reply; first_ms the first execute; steady_ms later ones.
 */
async function runKernel() {
  const report = { name: "kernel-start", error: null, heap: null };
  try {
    let t = performance.now();
    const { startBrowserKernel } = await import(new URL("dist/kernel/kernel.js", base).href);
    const workerUrl = new URL("dist/kernel/worker.js", base);
    const kernel = startBrowserKernel({
      createWorker: () => new Worker(workerUrl, { type: "module" }),
      log: () => {},
    });
    const replies = new Map();
    kernel.onMessage((channel, msg) => {
      const parent = msg.parent_header?.msg_id;
      const waiting = parent && replies.get(parent);
      if (!waiting) return;
      if (msg.header?.msg_type === "error") waiting.reject(new Error(msg.content.evalue));
      if (msg.header?.msg_type === waiting.type) {
        replies.delete(parent);
        waiting.resolve(msg);
      }
    });
    const send = (channel, msg, type) =>
      new Promise((resolve, reject) => {
        replies.set(msg.header.msg_id, { type, resolve, reject });
        kernel.send(channel, msg);
      });
    await send("shell", request("kernel_info_request", {}), "kernel_info_reply");
    report.start_ms = performance.now() - t;
    report.load_ms = 0;
    const times = [];
    for (let i = 0; i < repeats; i++) {
      t = performance.now();
      const msg = request("execute_request", { code: "1 + 1", silent: false, store_history: true, allow_stdin: false, stop_on_error: true, user_expressions: {} });
      const result = await send("shell", msg, "execute_result");
      times.push(performance.now() - t);
      report.result_preview = String(result.content.data["text/plain"]);
      report.result_bytes = report.result_preview.length;
    }
    report.first_ms = times[0];
    report.steady_ms = median(times.slice(1));
    report.cold_ms = report.start_ms + report.first_ms;
    void send("control", request("shutdown_request", { restart: false }), "shutdown_reply");
  } catch (e) {
    report.error = String(e.message ?? e);
  }
  return report;
}

const status = document.getElementById("status");
const manifest = JSON.parse(await text("workloads/manifest.json"));
const workloads = [];
for (const w of manifest.workloads) {
  status.textContent = `Running ${w.name}…`;
  workloads.push(await runWorkload(w));
}
status.textContent = "Running kernel-start…";
workloads.push(await runKernel());
const report = {
  engine: "worker",
  version: navigator.userAgent,
  jitless: false,
  init_ms: 0,
  workloads,
  max_rss_bytes: null,
};
window.benchResult = report;
status.textContent = "Done.";
document.getElementById("out").textContent = JSON.stringify(report, null, 2);
// The iOS bench app reads it from here (ios/Sources/WorkerBench.swift).
window.webkit?.messageHandlers?.bench?.postMessage(JSON.stringify(report));
