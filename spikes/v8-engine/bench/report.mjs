// Turns reports from v8-bench, node.mjs, web.mjs and the iOS bench app
// into Markdown tables.
//
//   node report.mjs [--title TEXT] report.json...
//
// A file may hold one report or an array of them (the iOS app's share).
import { readFileSync } from "node:fs";

const args = process.argv.slice(2);
let title = null;
if (args[0] === "--title") {
  title = args[1];
  args.splice(0, 2);
}
const reports = args.flatMap((f) => [JSON.parse(readFileSync(f, "utf8"))].flat()).filter(Boolean);
const label = (r) => {
  const engine = r.engine === "v8" ? `V8 ${r.version}` : r.engine;
  return `${engine}${r.jitless ? ", jitless" : ""}${r.device ? ` (${r.device})` : ""}`;
};
const ms = (x) => (x == null ? "–" : x < 10 ? x.toFixed(1) : Math.round(x).toString());
const mb = (x) => (x == null ? "–" : (x / 2 ** 20).toFixed(0));
const names = [...new Set(reports.flatMap((r) => r.workloads.map((w) => w.name)))];
const cell = (r, name, f) => {
  const w = r.workloads.find((w) => w.name === name);
  if (!w) return "–";
  if (w.error) return "error";
  return f(w);
};
const table = (head, rows) =>
  [`| ${head.join(" | ")} |`, `|${head.map(() => "---").join("|")}|`, ...rows.map((r) => `| ${r.join(" | ")} |`)].join("\n");

const out = [];
if (title) out.push(`### ${title}`, "");
out.push(
  "Cold start to first result, then the median of later runs, in ms (`cold / steady`):",
  "",
  table(
    ["workload", ...reports.map(label)],
    names.map((n) => [n, ...reports.map((r) => cell(r, n, (w) => `${ms(w.cold_ms)} / ${ms(w.steady_ms)}`))]),
  ),
  "",
  "Engine heap after the workload (physical MB), and the process's peak memory (resident set; on iOS, the footprint iOS counts against the app's limit):",
  "",
  table(
    ["workload", ...reports.map(label)],
    [
      ...names.map((n) => [n, ...reports.map((r) => cell(r, n, (w) => mb(w.heap?.physical_bytes)))]),
      ["peak RSS, whole run", ...reports.map((r) => mb(r.footprint_peak_bytes ?? r.max_rss_bytes))],
    ],
  ),
  "",
);
const errors = reports.flatMap((r) => r.workloads.filter((w) => w.error).map((w) => `- ${label(r)}, ${w.name}: ${w.error}`));
if (errors.length) out.push("Errors:", "", ...errors, "");
console.log(out.join("\n"));
