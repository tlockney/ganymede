#!/usr/bin/env node
// Runs the journaling eval set (#316) and prints the measures.
//
//   node evals/journal/run.mjs [--runs N] [--task ID] [--out DIR]
//   node evals/journal/run.mjs --rescore DIR
//
// Each session is `claude -p` in a fresh directory holding the task's
// data, with this repo's ganymede-journal plugin. `ganymede` must be on
// PATH (or set GANYMEDE_BIN), with the kernel the skill defaults to
// (`cdp-node`) installed. CLAUDE_BIN and CLAUDE_ARGS override the
// command; the default allows the ganymede tools and a few read-only
// shell commands.
//
// The notebook each session leaves is saved twice under DIR: `<run>.end.ipynb`,
// never touched again, and `<run>.ipynb`, for a person to read and edit.
// `--rescore DIR` scores again and tells whether the person rewrote the
// summary.

import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { measures, score } from "./score.mjs";
import { TASKS } from "./tasks.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const plugin = path.resolve(here, "../../plugins/ganymede-journal");

function args(argv) {
  const o = { runs: 1, task: null, out: null, rescore: null };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--runs") o.runs = Number(argv[++i]);
    else if (a === "--task") o.task = argv[++i];
    else if (a === "--out") o.out = argv[++i];
    else if (a === "--rescore") o.rescore = argv[++i];
    else throw new Error(`unknown argument: ${a}`);
  }
  return o;
}

const ALLOWED = [
  "mcp__plugin_ganymede-journal_ganymede",
  "Read",
  "Edit",
  "Write",
  "Bash(grep:*)",
  "Bash(wc:*)",
  "Bash(head:*)",
  "Bash(ls:*)",
].join(",");

function session(task, dir) {
  for (const [name, body] of Object.entries(task.files)) {
    fs.mkdirSync(path.dirname(path.join(dir, name)), { recursive: true });
    fs.writeFileSync(path.join(dir, name), body);
  }
  const bin = process.env.CLAUDE_BIN ?? "claude";
  const extra = process.env.CLAUDE_ARGS?.split(" ").filter(Boolean) ?? ["--allowedTools", ALLOWED];
  const env = { ...process.env };
  if (process.env.GANYMEDE_BIN) {
    env.PATH = `${path.dirname(path.resolve(process.env.GANYMEDE_BIN))}${path.delimiter}${env.PATH}`;
  }
  const r = spawnSync(bin, ["-p", task.prompt, "--plugin-dir", plugin, ...extra], {
    cwd: dir,
    env,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "inherit"],
  });
  if (r.error) throw r.error;
  return r.stdout;
}

/** The notebook a session left: the newest .ipynb under journal/, else anywhere. */
function journalOf(dir) {
  const found = [];
  const walk = (d) => {
    for (const e of fs.readdirSync(d, { withFileTypes: true })) {
      const p = path.join(d, e.name);
      if (e.isDirectory() && !e.name.startsWith(".")) walk(p);
      else if (e.name.endsWith(".ipynb")) found.push(p);
    }
  };
  walk(dir);
  found.sort((a, b) => fs.statSync(b).mtimeMs - fs.statSync(a).mtimeMs);
  return found.find((p) => p.includes(`${path.sep}journal${path.sep}`)) ?? found[0] ?? null;
}

const read = (p) => JSON.parse(fs.readFileSync(p, "utf8"));

function report(results) {
  for (const r of results) {
    console.log(`${r.run}: ${r.error ?? JSON.stringify(r.score)}`);
  }
  console.log(JSON.stringify(measures(results.filter((r) => r.score).map((r) => r.score)), null, 2));
}

function rescore(out) {
  const runs = JSON.parse(fs.readFileSync(path.join(out, "results.json"), "utf8"));
  const results = runs.map((r) => {
    const task = TASKS.find((t) => t.id === r.task);
    const end = path.join(out, `${r.run}.end.ipynb`);
    if (!fs.existsSync(end)) return { ...r, score: null };
    const now = path.join(out, `${r.run}.ipynb`);
    const reviewed = fs.existsSync(now) && fs.statSync(now).mtimeMs > fs.statSync(end).mtimeMs;
    return { ...r, score: score(read(end), reviewed ? read(now) : null, task) };
  });
  fs.writeFileSync(path.join(out, "results.json"), JSON.stringify(results, null, 2) + "\n");
  report(results);
}

function main() {
  const o = args(process.argv.slice(2));
  if (o.rescore) return rescore(o.rescore);
  const tasks = o.task ? TASKS.filter((t) => t.id === o.task) : TASKS;
  if (!tasks.length) throw new Error(`no task ${o.task}`);
  const out = o.out ?? fs.mkdtempSync(path.join(os.tmpdir(), "journal-evals-"));
  fs.mkdirSync(out, { recursive: true });
  const results = [];
  for (const task of tasks) {
    for (let n = 1; n <= o.runs; n++) {
      const run = `${task.id}-${n}`;
      const dir = fs.mkdtempSync(path.join(os.tmpdir(), `${run}-`));
      console.error(`${run}: ${dir}`);
      fs.writeFileSync(path.join(out, `${run}.log`), session(task, dir));
      const nb = journalOf(dir);
      if (!nb) {
        results.push({ run, task: task.id, error: "no notebook", score: null });
        continue;
      }
      fs.copyFileSync(nb, path.join(out, `${run}.end.ipynb`));
      fs.copyFileSync(nb, path.join(out, `${run}.ipynb`));
      results.push({ run, task: task.id, score: score(read(nb), null, task) });
    }
  }
  fs.writeFileSync(path.join(out, "results.json"), JSON.stringify(results, null, 2) + "\n");
  console.error(`results: ${out}`);
  report(results);
}

main();
