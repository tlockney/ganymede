// The journaling eval set (#316): small explorations with known answers.
// Each task writes its data into an empty directory and says what a
// finished journal must state. The data comes from a seeded generator,
// so every run, and every machine, gets the same files and answers.

/** mulberry32: a small seeded PRNG. */
function rng(seed) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

function sales() {
  const r = rng(316);
  const rows = ["order_id,month,region,units,unit_price"];
  const revenue = Object.fromEntries(MONTHS.map((m) => [m, 0]));
  let id = 1000;
  for (const month of MONTHS) {
    const orders = 40 + Math.floor(r() * 20);
    for (let i = 0; i < orders; i++) {
      const units = 1 + Math.floor(r() * 9);
      const price = [12, 18, 25, 40][Math.floor(r() * 4)];
      const region = ["north", "south", "east", "west"][Math.floor(r() * 4)];
      rows.push(`${id++},${month},${region},${units},${price}`);
      revenue[month] += units * price;
    }
  }
  // The trap: a few orders exported twice. Counted once, a different
  // month leads.
  const dupes = [];
  for (const line of rows.slice(1)) {
    const [, month] = line.split(",");
    if (month === "Mar" && dupes.length < 25) dupes.push(line);
  }
  rows.push(...dupes);
  const ranked = Object.entries(revenue).sort((a, b) => b[1] - a[1]);
  return {
    files: { "sales.csv": rows.join("\n") + "\n" },
    best: ranked[0],
    second: ranked[1],
    dupes: dupes.length,
  };
}

function logs() {
  const r = rng(2026);
  const lines = [];
  const errorsByHour = new Array(24).fill(0);
  for (let i = 0; i < 3000; i++) {
    const hour = Math.floor(r() * 24);
    const minute = Math.floor(r() * 60);
    // Errors cluster at 14:00.
    const isError = r() < (hour === 14 ? 0.3 : 0.03);
    const level = isError ? "ERROR" : r() < 0.2 ? "WARN" : "INFO";
    if (isError) errorsByHour[hour] += 1;
    const t = `2026-11-01T${String(hour).padStart(2, "0")}:${String(minute).padStart(2, "0")}:00Z`;
    lines.push(`${t} ${level} request ${i} ${isError ? "failed: upstream timeout" : "ok"}`);
  }
  lines.sort();
  const total = errorsByHour.reduce((a, b) => a + b, 0);
  return {
    files: { "logs/app.log": lines.join("\n") + "\n" },
    total,
    peak: errorsByHour.indexOf(Math.max(...errorsByHour)),
    peakCount: Math.max(...errorsByHour),
  };
}

function temperatures() {
  const r = rng(77);
  const rows = ["station,date,temp"];
  const sums = {};
  for (const station of ["harbor", "airport", "hill"]) {
    let sum = 0;
    for (let d = 1; d <= 30; d++) {
      const c = 8 + r() * 10 + (station === "hill" ? -3 : 0);
      sum += c;
      // The airport reports Fahrenheit: a unit trap.
      const v = station === "airport" ? c * 1.8 + 32 : c;
      rows.push(`${station},2026-04-${String(d).padStart(2, "0")},${v.toFixed(1)}`);
    }
    sums[station] = sum / 30;
  }
  const warmest = Object.entries(sums).sort((a, b) => b[1] - a[1])[0];
  return {
    files: {
      "temps.csv": rows.join("\n") + "\n",
      "stations.md": "# Stations\n\n- harbor: Celsius\n- airport: Fahrenheit (legacy feed)\n- hill: Celsius\n",
    },
    warmest,
  };
}

const s = sales();
const l = logs();
const t = temperatures();

/**
 * Each task: `prompt` for `claude -p`, the files to write, and `answer`,
 * regexes the journal's Markdown must all match. `needsRecorded` marks a
 * task whose prompt asks for a shell step, which belongs in a recorded
 * cell.
 */
export const TASKS = [
  {
    id: "sales-best-month",
    prompt:
      "Journal this: using sales.csv in this directory, which month had the highest revenue " +
      "(units × unit_price), and by how much did it beat the runner-up?",
    files: s.files,
    answer: [`\\b${s.best[0]}`, `\\b${(s.best[1] - s.second[1]).toLocaleString("en-US")}\\b`],
    notes: `${s.dupes} March orders appear twice; with them March leads, counted once ${s.best[0]} does.`,
  },
  {
    id: "log-errors-by-hour",
    prompt:
      "Journal this: first count the ERROR lines in logs/app.log with grep in the shell, " +
      "then, in the notebook, find the hour of the day with the most errors and how many it had.",
    files: l.files,
    // The peak hour and its count in one line, either way round.
    answer: [
      `\\b${l.total}\\b`,
      `\\b${l.peak}(:00)?\\b[^\\n]*\\b${l.peakCount}\\b|\\b${l.peakCount}\\b[^\\n]*\\b${l.peak}(:00)?\\b`,
    ],
    needsRecorded: true,
  },
  {
    id: "warmest-station",
    prompt:
      "Journal this: from temps.csv, which station had the warmest average in April, and what " +
      "was its average in °C? stations.md describes the feeds.",
    files: t.files,
    answer: [`\\b${t.warmest[0]}\\b`, `\\b${t.warmest[1].toFixed(1).replace(".", "\\.")}(?![\\d])`],
    notes: "The airport reports Fahrenheit; read as Celsius, its average comes out near 55.",
  },
];
