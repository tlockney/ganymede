// A large output: a 100,000-row CSV (about 3.3 MB) built as a string and
// handed to the host, as a cell's text/plain or text/csv output would be.
globalThis.workload = {
  run() {
    const lines = ["id,name,x,y,ok"];
    for (let i = 0; i < 100000; i++) {
      lines.push(i + ",row" + i + "," + (i * 0.5).toFixed(2) + "," + ((i * 7) % 1000) + "," + (i % 3 === 0));
    }
    return lines.join("\n");
  },
};
