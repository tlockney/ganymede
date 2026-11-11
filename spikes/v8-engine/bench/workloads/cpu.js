// A CPU-bound cell: recursion, sorting, Map and string work, JSON, regex.
// Deterministic, so every engine returns the same checksum.
globalThis.workload = {
  run() {
    let seed = 42;
    const rand = () => {
      seed = (seed + 0x6d2b79f5) | 0;
      let t = Math.imul(seed ^ (seed >>> 15), 1 | seed);
      t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
      return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
    };
    const fib = (n) => (n < 2 ? n : fib(n - 1) + fib(n - 2));
    const f = fib(24);

    const xs = Array.from({ length: 100000 }, rand);
    xs.sort((a, b) => a - b);

    const words = ["alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "theta"];
    const counts = new Map();
    let text = "";
    for (let i = 0; i < 50000; i++) {
      const w = words[Math.floor(rand() * words.length)];
      counts.set(w, (counts.get(w) ?? 0) + 1);
      if (i < 20000) text += w + " ";
    }
    const replaced = text.replace(/\b(\w)(\w*)\b/g, (_, a, b) => a.toUpperCase() + b);

    const rows = Array.from({ length: 10000 }, (_, i) => ({ id: i, name: "row" + i, v: xs[i] }));
    const back = JSON.parse(JSON.stringify(rows));

    let sum = 0;
    for (const r of back) sum += r.v;
    return [f, xs[500].toFixed(6), counts.get("gamma"), replaced.length, back.length, sum.toFixed(3)].join(",");
  },
};
