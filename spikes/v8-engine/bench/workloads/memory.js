// Peak memory: half a million small objects and a 16 MB typed array,
// kept alive in a global, as a cell that loads a dataset would.
globalThis.workload = {
  run() {
    const rows = new Array(500000);
    for (let i = 0; i < rows.length; i++) rows[i] = { id: i, name: "row" + i, values: [i, i * 2] };
    const buf = new Float64Array(2 * 1024 * 1024);
    for (let i = 0; i < buf.length; i++) buf[i] = i;
    globalThis.kept = { rows, buf };
    return String(rows.length + buf.length);
  },
};
