#!/usr/bin/env node
// Review replay eval (#822): runs the pre-push review prompt on each case,
// has a judge score the reviews against the known findings, writes a report.
//
//   node evals/review/run.mjs --prompt PATH [--runs N] [--cases IDS] [--lens NAME]
//        [--out DIR] [--model opus] [--judge-model sonnet] [--concurrency 2]
//        [--cache DIR] [--redo]
//   node evals/review/run.mjs --report DIR      (rebuild the report from DIR/results.json)
//
// See README.md.

import { execFileSync } from "node:child_process";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { assemblePrompt, branchOf, loadCases, selectCases, LENSES } from "./cases.mjs";
import { runClaude } from "./claude.mjs";
import { judgeReview } from "./judge.mjs";
import { aggregate, lensComparison, renderReport, reviewVerdict } from "./score.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));

export function parseArgs(argv) {
  const o = {
    prompt: null, runs: null, cases: null, lens: null, out: null, model: "opus", judgeModel: "sonnet",
    concurrency: 2, cache: process.env.REVIEW_EVAL_CACHE ?? path.join(os.homedir(), ".cache", "ganymede-review-eval"),
    redo: false, report: null,
  };
  const val = (i, a) => {
    if (i + 1 >= argv.length) throw new Error(`${a} needs a value`);
    return argv[i + 1];
  };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--prompt") o.prompt = val(i++, a);
    else if (a === "--runs") o.runs = Number(val(i++, a));
    else if (a === "--cases") o.cases = val(i++, a);
    else if (a === "--lens") o.lens = val(i++, a);
    else if (a === "--out") o.out = val(i++, a);
    else if (a === "--model") o.model = val(i++, a);
    else if (a === "--judge-model") o.judgeModel = val(i++, a);
    else if (a === "--concurrency") o.concurrency = Number(val(i++, a));
    else if (a === "--cache") o.cache = val(i++, a);
    else if (a === "--report") o.report = val(i++, a);
    else if (a === "--redo") o.redo = true;
    else throw new Error(`unknown argument: ${a}`);
  }
  if (o.lens && !(o.lens in LENSES)) throw new Error(`unknown lens ${o.lens} (use ${Object.keys(LENSES).join(" or ")})`);
  if (o.runs == null) o.runs = o.lens ? 1 : 2;
  if (!Number.isInteger(o.runs) || o.runs < 1) throw new Error("--runs must be a positive integer");
  if (!Number.isInteger(o.concurrency) || o.concurrency < 1) throw new Error("--concurrency must be a positive integer");
  if (!o.report && !o.prompt) throw new Error("--prompt PATH is required (the review-prompt.md to test)");
  return o;
}

// Read-only tools: file reads, search, git reads, the issue text, and the
// skill the prompt asks for. No edits, no builds, no GitHub PR or API reads
// (they would show the posted review).
const GIT_READ = ["log", "show", "diff", "status", "blame", "ls-files", "grep", "cat-file", "rev-parse", "merge-base", "ls-tree", "show-ref", "describe"];
export const ALLOWED = ["Read", "Grep", "Glob", "Skill", "Agent", "Task", ...GIT_READ.map((g) => `Bash(git ${g}:*)`),
  "Bash(gh issue view:*)", "Bash(ls:*)", "Bash(cat:*)", "Bash(head:*)", "Bash(tail:*)", "Bash(wc:*)", "Bash(find:*)"].join(",");
export const DISALLOWED = ["Edit", "Write", "NotebookEdit", "WebFetch", "WebSearch", "Bash(gh api:*)", "Bash(gh pr:*)", "Bash(curl:*)", "Bash(git push:*)", "Bash(git commit:*)", "Bash(git checkout:*)", "Bash(git diff --output:*)"].join(",");

const git = (dir, ...a) => execFileSync("git", ["-C", dir, ...a], { encoding: "utf8", maxBuffer: 256 * 1024 * 1024 });

/** The bare clone for a repo, with the case's commits present. */
function ensureRepo(cache, c) {
  const name = c.repo.split("/")[1];
  const dir = path.join(cache, `${name}.git`);
  fs.mkdirSync(cache, { recursive: true });
  if (!fs.existsSync(dir)) {
    execFileSync("git", ["clone", "--bare", "-q", `https://github.com/${c.repo}.git`, dir], { stdio: "inherit" });
    git(dir, "config", "--add", "remote.origin.fetch", "+refs/pull/*/head:refs/pull/*/head");
  }
  const has = (sha) => { try { git(dir, "cat-file", "-e", `${sha}^{commit}`); return true; } catch { return false; } };
  for (const sha of [c.base, c.head]) {
    if (!has(sha)) git(dir, "fetch", "-q", "origin", sha);
    if (!has(sha)) throw new Error(`${c.id}: commit ${sha} not found in ${c.repo}`);
  }
  return dir;
}

/**
 * A checkout of the case's head for one run, sharing objects with the cache.
 * It looks like the author's clone at review time: the branch at head and
 * `origin/main` at the base, so `git diff origin/main...HEAD` is the case's
 * diff and the history after the change is not reachable from any ref.
 */
function checkout(bare, c, dest) {
  fs.mkdirSync(dest, { recursive: true });
  git(dest, "init", "-q");
  fs.writeFileSync(path.join(dest, ".git", "objects", "info", "alternates"), path.join(bare, "objects") + "\n");
  git(dest, "remote", "add", "origin", `https://github.com/${c.repo}.git`);
  git(dest, "update-ref", "refs/remotes/origin/main", c.base);
  git(dest, "update-ref", `refs/heads/${branchOf(c)}`, c.head);
  git(dest, "symbolic-ref", "HEAD", `refs/heads/${branchOf(c)}`);
  git(dest, "reset", "-q", "--hard");
}

const key = (e) => `${e.case}|${e.lens ?? ""}|${e.run}`;

function readResults(out) {
  try {
    return JSON.parse(fs.readFileSync(path.join(out, "results.json"), "utf8"));
  } catch {
    return [];
  }
}

function writeResults(out, entries) {
  fs.writeFileSync(path.join(out, "results.json"), JSON.stringify(entries, null, 2) + "\n");
}

/** Summary, report.md and summary.json for everything in results.json. */
export function writeReport(out, cases, meta) {
  const entries = readResults(out).filter((e) => cases.some((c) => c.id === e.case));
  meta = { ...meta, runs: Math.max(0, ...entries.filter((e) => !e.lens).map((e) => e.run)) };
  const summary = { agg: aggregate(entries, cases), lens: lensComparison(entries, cases) };
  const { rows, ...lens } = summary.lens;
  fs.writeFileSync(path.join(out, "summary.json"), JSON.stringify({ meta, ...summary, lens }, null, 2) + "\n");
  fs.writeFileSync(path.join(out, "report.md"), renderReport(summary, entries, cases, meta));
  return summary;
}

async function pool(tasks, n) {
  let next = 0;
  await Promise.all(Array.from({ length: n }, async () => {
    while (next < tasks.length) await tasks[next++]();
  }));
}

async function main() {
  const o = parseArgs(process.argv.slice(2));
  const allCases = loadCases(path.join(here, "cases"));
  const out = path.resolve(o.report ?? o.out ?? fs.mkdtempSync(path.join(os.tmpdir(), "review-eval-")));
  fs.mkdirSync(path.join(out, "reviews"), { recursive: true });
  const meta = (promptPath, text) => ({
    date: new Date().toISOString().slice(0, 10), prompt: promptPath, promptSha: crypto.createHash("sha256").update(text).digest("hex"),
    model: o.model, judgeModel: o.judgeModel, runs: o.runs,
  });
  if (o.report) {
    const old = readResults(out);
    const m = fs.existsSync(path.join(out, "summary.json")) ? JSON.parse(fs.readFileSync(path.join(out, "summary.json"), "utf8")).meta : meta("unknown", "");
    writeReport(out, allCases, m);
    console.error(`report: ${path.join(out, "report.md")} (${old.length} entries)`);
    return;
  }
  const promptText = fs.readFileSync(path.resolve(o.prompt), "utf8");
  let cases = selectCases(allCases, o.cases);
  if (o.lens && !o.cases) cases = cases.filter((c) => c.kind !== "clean" && c.findings.some((f) => f.class === "A" || f.class === "B"));
  const entries = readResults(out);
  const done = new Set(entries.filter((e) => !e.error && e.judge).map(key));
  const timeoutMs = Number(process.env.EVAL_TIMEOUT_MS ?? 60 * 60 * 1000);
  const work = path.join(o.cache, "work");
  const tasks = [];
  for (const c of cases) {
    for (let run = 1; run <= o.runs; run++) {
      const e = { case: c.id, lens: o.lens, run };
      if (!o.redo && done.has(key(e))) continue;
      tasks.push(async () => {
        const tag = `${c.id}${o.lens ? `-${o.lens}` : ""}-${run}`;
        const entry = { ...e };
        const dest = path.join(work, `${tag}-${process.pid}`);
        try {
          const bare = ensureRepo(o.cache, c);
          const diff = git(bare, "diff", `${c.base}...${c.head}`);
          if (!diff.trim()) throw new Error("empty diff");
          const input = assemblePrompt(promptText, { lens: o.lens, repo: c.repo, clone: dest, branch: branchOf(c), issue: c.issue, diff });
          let r;
          for (let attempt = 0; attempt < 2; attempt++) {
            fs.rmSync(dest, { recursive: true, force: true });
            checkout(bare, c, dest);
            console.error(`${tag}: review${attempt ? " (retry)" : ""}`);
            r = await runClaude({ args: ["--model", o.model, "--permission-mode", "dontAsk", "--allowedTools", ALLOWED, "--disallowedTools", DISALLOWED], input, cwd: dest, timeoutMs });
            entry.cost = (entry.cost ?? 0) + r.cost;
            if (r.code === 0 && !r.isError && r.text.trim()) break;
          }
          entry.usage = r.usage;
          entry.durationMs = r.durationMs;
          if (r.code !== 0 || r.isError || !r.text.trim()) throw new Error(`claude exited ${r.code}${r.timedOut ? " (timed out)" : ""}: ${(r.stderr || r.raw || "").slice(-300)}`);
          const file = path.join("reviews", `${tag}.md`);
          fs.writeFileSync(path.join(out, file), r.text);
          entry.review = file;
          entry.verdict = reviewVerdict(r.text);
          console.error(`${tag}: judge`);
          const j = await judgeReview({ findings: c.findings, reviewText: r.text, model: o.judgeModel });
          entry.judgeCost = j.cost;
          if (!j.ok) throw new Error(`judge: ${j.error}`);
          entry.judge = { perFinding: j.perFinding, otherMediumPlus: j.otherMediumPlus };
        } catch (err) {
          entry.error = String(err.message ?? err);
          console.error(`${tag}: FAILED ${entry.error}`);
        } finally {
          fs.rmSync(dest, { recursive: true, force: true });
        }
        const all = readResults(out).filter((x) => key(x) !== key(entry));
        all.push(entry);
        writeResults(out, all);
        writeReport(out, allCases, meta(o.prompt, promptText));
      });
    }
  }
  console.error(`${tasks.length} runs, concurrency ${o.concurrency}, out ${out}`);
  await pool(tasks, o.concurrency);
  const s = writeReport(out, allCases, meta(o.prompt, promptText));
  console.error(`report: ${path.join(out, "report.md")}`);
  console.log(JSON.stringify({ recall: s.agg.recall, falseAlarms: s.agg.falseAlarms.perRun, cost: s.agg.cost.total }));
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch((e) => {
    console.error(e.message);
    process.exit(1);
  });
}
