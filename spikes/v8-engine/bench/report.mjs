// Turns reports from v8-bench, node.mjs, web.mjs and the iOS bench app
// into Markdown tables.
//
//   node report.mjs [--title TEXT] report.json...
//
// Several reports of one engine (same label) are runs of it; the tables
// show their medians.
//
// A file may hold one report or an array of them (the iOS app's share).
import { readFileSync } from "node:fs";

const args = process.argv.slice(2);
let title = null;
if (args[0] === "--title") {
  title = args[1];
  args.splice(0, 2);
}
const all = args.flatMap((f) => [JSON.parse(readFileSync(f, "utf8"))].flat().map((r) => ({ file: f, r })));
// A report without workloads (a bad config, or the iOS app's error path)
// carries only `error`: listed at the end, kept out of the tables.
const failed = all.filter(({ r }) => r && !Array.isArray(r.workloads));
const reports = all.filter(({ r }) => r && Array.isArray(r.workloads)).map(({ r }) => r);
const label = (r) => {
  const engine = r.engine === "v8" ? `V8 ${r.version}` : r.engine;
  return `${engine}${r.jitless ? ", jitless" : ""}${r.device ? ` (${r.device})` : ""}`;
};
// Reports with the same label are samples of one engine: each cell is
// their median, so a single noisy run doesn't decide it.
const columns = [];
for (const r of reports) {
  const col = columns.find((c) => c.label === label(r));
  if (col) col.samples.push(r);
  else columns.push({ label: label(r), samples: [r] });
}
const median = (xs) => {
  const s = xs.filter((x) => typeof x === "number").sort((a, b) => a - b);
  if (!s.length) return null;
  const n = s.length;
  return n % 2 ? s[(n - 1) / 2] : (s[n / 2 - 1] + s[n / 2]) / 2;
};
const ms = (x) => (x == null ? "–" : x < 10 ? x.toFixed(1) : Math.round(x).toString());
const mb = (x) => (x == null ? "–" : (x / 2 ** 20).toFixed(0));
const names = [...new Set(reports.flatMap((r) => r.workloads.map((w) => w.name)))];
/** The median of f over the column's samples of workload `name`. */
const cell = (col, name, f) => {
  const ws = col.samples.map((r) => r.workloads.find((w) => w.name === name)).filter(Boolean);
  if (!ws.length) return null;
  if (ws.some((w) => w.error)) return "error";
  return median(ws.map(f));
};
const show = (x, fmt) => (x === "error" ? "error" : fmt(x));
const head = ["workload", ...columns.map((c) => (c.samples.length > 1 ? `${c.label}, ${c.samples.length} runs` : c.label))];
const table = (rows) =>
  [`| ${head.join(" | ")} |`, `|${head.map(() => "---").join("|")}|`, ...rows.map((r) => `| ${r.join(" | ")} |`)].join("\n");

const out = [];
if (title) out.push(`### ${title}`, "");
out.push(
  "Cold start to first result, then the median of later runs, in ms (`cold / steady`; with several runs, the median of each). " +
    "`init` is the engine's one-time setup per process, which a first kernel start adds to `start`:",
  "",
  table([
    ["init, once per process", ...columns.map((c) => ms(median(c.samples.map((r) => r.init_ms || null))))],
    ...names.map((n) => [
      n,
      ...columns.map((c) => {
        const cold = cell(c, n, (w) => w.cold_ms);
        if (cold === "error") return "error";
        if (cold == null) return "–";
        return `${ms(cold)} / ${ms(cell(c, n, (w) => w.steady_ms))}`;
      }),
    ]),
  ]),
  "",
  "Engine heap after the workload (physical MB), and the process's peak memory (resident set; on iOS, the footprint iOS counts against the app's limit):",
  "",
  table([
    ...names.map((n) => [n, ...columns.map((c) => show(cell(c, n, (w) => w.heap?.physical_bytes), mb))]),
    ["peak RSS, whole run", ...columns.map((c) => mb(median(c.samples.map((r) => r.footprint_peak_bytes ?? r.max_rss_bytes))))],
  ]),
  "",
);
const errors = reports.flatMap((r) => r.workloads.filter((w) => w.error).map((w) => `- ${label(r)}, ${w.name}: ${w.error}`));
errors.push(...failed.map(({ file, r }) => `- ${file}: ${r.error ?? "no workloads"}`));
if (errors.length) out.push("Errors:", "", ...errors, "");
console.log(out.join("\n"));
