// The Worker side of bench.js: load the sources, then answer "run".
// A classic Worker, as cdp-kernel's cells see globalThis.
self.postMessage({ op: "ready" });
self.onmessage = (e) => {
  const m = e.data;
  try {
    if (m.op === "load") {
      for (const src of m.sources) (0, eval)(src);
      self.postMessage({ op: "loaded" });
    } else if (m.op === "run") {
      self.postMessage({ op: "result", result: globalThis.workload.run() });
    }
  } catch (err) {
    self.postMessage({ op: "error", error: String((err && err.stack) || err) });
  }
};
