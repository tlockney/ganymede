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
import { evalTimeoutMs, runClaude } from "./claude.mjs";
import { mcpConfig } from "./issue-mcp.mjs";
import { judgeReview } from "./judge.mjs";
import { afterRedo, entryKey, lock, mismatched, readResults, writeResults } from "./store.mjs";
import { allowedTools, assertWorkAllowed, denyPaths, disallowedTools } from "./tools.mjs";
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

const git = (dir, ...a) => execFileSync("git", ["-C", dir, ...a], { encoding: "utf8", maxBuffer: 256 * 1024 * 1024 });

/** The bare clone for a repo, with the case's commits present. */
export function ensureRepo(cache, c) {
  const name = c.repo.split("/")[1];
  const dir = path.join(cache, `${name}.git`);
  fs.mkdirSync(cache, { recursive: true });
  if (!fs.existsSync(dir)) execFileSync("git", ["clone", "--bare", "-q", `https://github.com/${c.repo}.git`, dir], { stdio: "inherit" });
  // Scratch clones fetch the case's two commits from here by sha.
  git(dir, "config", "uploadpack.allowAnySHA1InWant", "true");
  const has = (sha) => { try { git(dir, "cat-file", "-e", `${sha}^{commit}`); return true; } catch { return false; } };
  for (const sha of [c.base, c.head]) {
    if (!has(sha)) git(dir, "fetch", "-q", "origin", sha);
    if (!has(sha)) throw new Error(`${c.id}: commit ${sha} not found in ${c.repo}`);
  }
  return dir;
}

/**
 * A checkout of the case's head for one run. It holds only the history of
 * `base` and `head` (fetched into its own objects, no alternates), so a later
 * commit cannot be read by sha. The branch is at `head` and `origin/main` at
 * `base`, so `git diff origin/main...HEAD` is the case's diff.
 */
export function checkout(bare, c, dest) {
  fs.mkdirSync(dest, { recursive: true });
  git(dest, "init", "-q");
  git(dest, "remote", "add", "origin", `https://github.com/${c.repo}.git`);
  git(dest, "fetch", "-q", "--no-tags", bare, c.base, c.head);
  git(dest, "update-ref", "refs/remotes/origin/main", c.base);
  git(dest, "update-ref", `refs/heads/${branchOf(c)}`, c.head);
  git(dest, "symbolic-ref", "HEAD", `refs/heads/${branchOf(c)}`);
  git(dest, "reset", "-q", "--hard");
  fs.rmSync(path.join(dest, ".git", "FETCH_HEAD"), { force: true });
}

/** Removes scratch checkouts left by runs that died: `<tag>-<pid>` with no live pid. */
function clearStaleWork(work) {
  if (!fs.existsSync(work)) return;
  for (const d of fs.readdirSync(work)) {
    const pid = Number(d.match(/-(\d+)$/)?.[1]);
    let alive = false;
    try { alive = pid > 0 && process.kill(pid, 0); } catch (e) { alive = e.code === "EPERM"; }
    if (!alive) fs.rmSync(path.join(work, d), { recursive: true, force: true });
  }
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

/** What a review run needs from outside: the model call and the judge. Tests pass stubs. */
const REAL = { run: runClaude, judge: judgeReview };

/**
 * Runs the eval for options `o` (see parseArgs) and writes the report.
 * `deps` replace the model calls and `casesDir` the case files, for tests.
 */
export async function runEval(o, { run: runModel, judge, casesDir = path.join(here, "cases") } = REAL) {
  const allCases = loadCases(casesDir);
  const out = path.resolve(o.report ?? o.out ?? fs.mkdtempSync(path.join(os.tmpdir(), "review-eval-")));
  fs.mkdirSync(path.join(out, "reviews"), { recursive: true });
  const meta = (promptPath, text) => ({
    date: new Date().toISOString().slice(0, 10), prompt: promptPath, promptSha: crypto.createHash("sha256").update(text).digest("hex"),
    model: o.model, judgeModel: o.judgeModel, runs: o.runs,
  });
  if (o.report) {
    const release = lock(out);
    const m = fs.existsSync(path.join(out, "summary.json")) ? JSON.parse(fs.readFileSync(path.join(out, "summary.json"), "utf8")).meta : meta("unknown", "");
    writeReport(out, allCases, m);
    release();
    console.error(`report: ${path.join(out, "report.md")}`);
    return null;
  }
  const release = lock(out);
  try {
    const promptText = fs.readFileSync(path.resolve(o.prompt), "utf8");
    const m = meta(o.prompt, promptText);
    let cases = selectCases(allCases, o.cases);
    if (o.lens && !o.cases) cases = cases.filter((c) => c.kind !== "clean" && c.findings.some((f) => f.class === "A" || f.class === "B"));
    const work = path.join(o.cache, "work");
    const deny = denyPaths({ cache: o.cache, out, repoRoot: path.resolve(here, "../..") });
    assertWorkAllowed(work, deny);
    const selected = new Set(cases.flatMap((c) => Array.from({ length: o.runs }, (_, i) => entryKey({ case: c.id, lens: o.lens, run: i + 1 }))));
    let existing = readResults(out);
    const bad = mismatched(existing, m);
    if (bad.length && !o.redo) throw new Error(`${out} holds ${bad.length} runs made with another prompt or model (e.g. ${bad[0].case} ${bad[0].promptSha?.slice(0, 8)} ${bad[0].model}); use a new --out, or --redo to run the selected cases again and drop the rest of them`);
    if (o.redo) {
      existing = afterRedo(existing, m, selected);
      writeResults(out, existing);
    }
    const prior = new Map(existing.map((e) => [entryKey(e), e]));
    const timeoutMs = evalTimeoutMs(60 * 60 * 1000);
    clearStaleWork(work);
    const tasks = [];
    for (const c of cases) {
      for (let n = 1; n <= o.runs; n++) {
        const e = { case: c.id, lens: o.lens, run: n };
        const old = prior.get(entryKey(e));
        if (old && !old.error && old.judge) continue;
        tasks.push(async () => {
          const tag = `${c.id}${o.lens ? `-${o.lens}` : ""}-${n}`;
          const entry = { ...e, promptSha: m.promptSha, model: o.model, judgeModel: o.judgeModel };
          const dest = path.join(work, `${tag}-${process.pid}`);
          try {
            let text;
            // A review that was saved and only failed to judge is judged again, not run again.
            if (old?.review && fs.existsSync(path.join(out, old.review))) {
              console.error(`${tag}: re-judging the saved review`);
              text = fs.readFileSync(path.join(out, old.review), "utf8");
              Object.assign(entry, { review: old.review, verdict: old.verdict, cost: old.cost, usage: old.usage, durationMs: old.durationMs, denials: old.denials });
            } else {
              const bare = ensureRepo(o.cache, c);
              const diff = git(bare, "diff", `${c.base}...${c.head}`);
              if (!diff.trim()) throw new Error("empty diff");
              const input = assemblePrompt(promptText, { lens: o.lens, repo: c.repo, clone: dest, branch: branchOf(c), issue: c.issue, diff });
              const args = ["--model", o.model, "--permission-mode", "dontAsk", "--setting-sources", "", "--strict-mcp-config", "--mcp-config", mcpConfig(c), "--allowedTools", allowedTools(c).join(","), "--disallowedTools", disallowedTools(deny).join(",")];
              let r;
              for (let attempt = 0; attempt < 2; attempt++) {
                fs.rmSync(dest, { recursive: true, force: true });
                checkout(bare, c, dest);
                console.error(`${tag}: review${attempt ? " (retry)" : ""}`);
                r = await runModel({ args, input, cwd: dest, timeoutMs });
                entry.cost = (entry.cost ?? 0) + r.cost;
                if (r.code === 0 && !r.isError && r.text.trim()) break;
              }
              entry.usage = r.usage;
              entry.denials = (r.denials ?? []).length;
              entry.durationMs = r.durationMs;
              if (r.code !== 0 || r.isError || !r.text.trim()) throw new Error(`claude exited ${r.code}${r.timedOut ? " (timed out)" : ""}: ${(r.stderr || r.raw || "").slice(-300)}`);
              const file = path.join("reviews", `${tag}.md`);
              fs.writeFileSync(path.join(out, file), r.text);
              entry.review = file;
              entry.verdict = reviewVerdict(r.text);
              text = r.text;
            }
            console.error(`${tag}: judge`);
            const j = await judge({ findings: c.findings, reviewText: text, model: o.judgeModel });
            entry.judgeCost = (old?.judgeCost ?? 0) + (j.cost ?? 0);
            if (!j.ok) throw new Error(`judge: ${j.error}`);
            entry.judge = { perFinding: j.perFinding, otherMediumPlus: j.otherMediumPlus };
          } catch (err) {
            entry.error = String(err.message ?? err);
            console.error(`${tag}: FAILED ${entry.error}`);
          } finally {
            fs.rmSync(dest, { recursive: true, force: true });
          }
          const all = readResults(out).filter((x) => entryKey(x) !== entryKey(entry));
          all.push(entry);
          writeResults(out, all);
          writeReport(out, allCases, m);
        });
      }
    }
    console.error(`${tasks.length} runs, concurrency ${o.concurrency}, out ${out}`);
    await pool(tasks, o.concurrency);
    const s = writeReport(out, allCases, m);
    console.error(`report: ${path.join(out, "report.md")}`);
    return s;
  } finally {
    release();
  }
}

async function main() {
  const s = await runEval(parseArgs(process.argv.slice(2)));
  if (s) console.log(JSON.stringify({ recall: s.agg.recall, falseAlarms: s.agg.falseAlarms.perRun, cost: s.agg.cost.total }));
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch((e) => {
    console.error(e.message);
    process.exit(1);
  });
}
