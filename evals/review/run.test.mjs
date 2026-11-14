// node --test evals/review/*.test.mjs
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { judgeReview } from "./judge.mjs";
import { checkIssues, checkout, clearStaleWork, runEval } from "./run.mjs";
import { readResults } from "./store.mjs";

const git = (dir, ...a) => execFileSync("git", ["-C", dir, ...a], { encoding: "utf8" }).trim();
const made = [];
process.on("exit", () => made.forEach((d) => fs.rmSync(d, { recursive: true, force: true })));
const tmp = () => (made.push(fs.mkdtempSync(path.join(os.tmpdir(), "review-run-"))), made.at(-1));

/** A bare repo `<cache>/rtest.git` with three commits; returns shas. */
function fixture() {
  const root = tmp();
  const cache = path.join(root, "cache");
  const src = path.join(root, "src");
  fs.mkdirSync(src, { recursive: true });
  git(src, "init", "-q", "-b", "main");
  git(src, "config", "user.email", "t@example.test");
  git(src, "config", "user.name", "t");
  const shas = [];
  for (const n of [1, 2, 3]) {
    fs.writeFileSync(path.join(src, "f.txt"), `v${n}\n`);
    git(src, "add", ".");
    git(src, "commit", "-q", "-m", `c${n}`);
    shas.push(git(src, "rev-parse", "HEAD"));
  }
  fs.mkdirSync(cache, { recursive: true });
  const bare = path.join(cache, "rtest.git");
  execFileSync("git", ["clone", "--bare", "-q", src, bare]);
  git(bare, "config", "uploadpack.allowAnySHA1InWant", "true");
  return { root, cache, bare, shas };
}

const kase = (shas) => ({ id: "rtest-1", repo: "tlockney/rtest", pr: 1, issue: 2, base: shas[0], head: shas[1], branch: "feature", kind: "known", notes: "",
  findings: [{ summary: "x".repeat(50), severity: "Medium", class: "A", source: "s" }] });

test("a scratch checkout holds base and head only: no later commit, no alternates, the case's diff", () => {
  const { root, bare, shas } = fixture();
  const c = kase(shas);
  const dest = path.join(root, "work", "x");
  checkout(bare, c, dest);
  assert.equal(git(dest, "rev-parse", "HEAD"), shas[1]);
  assert.equal(git(dest, "rev-parse", "origin/main"), shas[0]);
  assert.equal(git(dest, "branch", "--show-current"), "feature");
  assert.equal(fs.existsSync(path.join(dest, ".git", "objects", "info", "alternates")), false);
  assert.throws(() => git(dest, "cat-file", "-e", `${shas[2]}^{commit}`), /./, "the later commit is not there");
  assert.throws(() => git(dest, "show", shas[2]), /./);
  assert.equal(git(dest, "diff", "origin/main...HEAD"), git(bare, "diff", `${shas[0]}...${shas[1]}`));
  assert.equal(fs.readFileSync(path.join(dest, "f.txt"), "utf8"), "v2\n");
});

test("resume: a finished run is kept, a saved review that failed to judge is judged again, not rerun", async () => {
  const { root, cache, shas } = fixture();
  const casesDir = path.join(root, "cases");
  fs.mkdirSync(casesDir);
  fs.writeFileSync(path.join(casesDir, "rtest-1.json"), JSON.stringify(kase(shas)));
  const prompt = path.join(root, "prompt.md");
  fs.writeFileSync(prompt, "# Prompt\n");
  const out = path.join(root, "out");
  const o = { prompt, runs: 1, cases: null, lens: null, out, model: "opus", judgeModel: "sonnet", concurrency: 1, cache, redo: false, report: null };
  let reviews = 0;
  let judges = 0;
  const stubRun = async ({ input, cwd }) => {
    reviews++;
    assert.ok(input.startsWith("# Prompt\n"));
    assert.ok(input.includes("BRANCH: feature") && input.includes("ISSUE: 2"));
    assert.ok(fs.existsSync(path.join(cwd, "f.txt")), "runs in the checkout");
    return { code: 0, isError: false, text: "A review.\nReview: pass\n", cost: 1, usage: {}, durationMs: 1, denials: [1, 2] };
  };
  const failingJudge = async () => (judges++, { ok: false, error: "garbled", cost: 0.1 });
  const goodJudge = async () => (judges++, { ok: true, perFinding: [{ verdict: "caught", evidence: "" }], otherMediumPlus: 0, cost: 0.1 });
  const checked = [];
  const check = (cs) => checked.push(...cs.map((c) => c.id));
  const log = console.error;
  console.error = () => {};
  try {
    await runEval(o, { run: stubRun, judge: failingJudge, casesDir, check });
    let [e] = readResults(out);
    assert.match(e.error, /judge: garbled/);
    assert.equal(e.review, path.join("reviews", "rtest-1-1.md"));
    assert.equal(reviews, 1);
    await runEval(o, { run: stubRun, judge: goodJudge, casesDir, check });
    [e] = readResults(out);
    assert.equal(reviews, 1, "the saved review was not run again");
    assert.equal(e.error, undefined);
    assert.equal(e.judge.perFinding[0].verdict, "caught");
    assert.equal(e.cost, 1, "the review's cost is kept");
    assert.equal(e.denials, 2);
    assert.equal(e.verdict, "pass");
    await runEval(o, { run: stubRun, judge: goodJudge, casesDir, check });
    assert.equal(judges, 2, "a judged run is left alone");
    // another prompt is refused; --redo with it runs the selection again
    fs.writeFileSync(prompt, "# Another prompt\n");
    await assert.rejects(runEval(o, { run: stubRun, judge: goodJudge, casesDir, check }), /another prompt or model/);
    await runEval({ ...o, redo: true }, { run: stubRun, judge: goodJudge, casesDir, check });
    assert.equal(reviews, 2);
    assert.equal(readResults(out).length, 1);
    assert.ok(fs.existsSync(path.join(out, "report.md")));
    assert.deepEqual(checked, ["rtest-1", "rtest-1", "rtest-1"], "the issue check ran for each invocation with work to do");
  } finally {
    console.error = log;
  }
});

const quiet = async (f) => {
  const log = console.error;
  console.error = () => {};
  try {
    return await f();
  } finally {
    console.error = log;
  }
};

function setup(cases) {
  const { root, cache, shas } = fixture();
  const casesDir = path.join(root, "cases");
  fs.mkdirSync(casesDir);
  for (const c of cases(shas)) fs.writeFileSync(path.join(casesDir, `${c.id}.json`), JSON.stringify(c));
  const prompt = path.join(root, "prompt.md");
  fs.writeFileSync(prompt, "# Prompt\n");
  const o = { prompt, runs: 1, cases: null, lens: null, out: path.join(root, "out"), model: "opus", judgeModel: "sonnet", concurrency: 1, cache, redo: false, report: null };
  return { root, casesDir, o };
}
const okJudge = async () => ({ ok: true, perFinding: [{ verdict: "missed", evidence: "" }], otherMediumPlus: 0, cost: 0 });
const none = () => {};

test("a run that exits non-zero is retried once and the costs add up", async () => {
  const { casesDir, o } = setup((shas) => [kase(shas)]);
  let calls = 0;
  const run = async () => (++calls === 1 ? { code: 1, isError: true, text: "", cost: 0.5, stderr: "boom" } : { code: 0, isError: false, text: "Review: pass", cost: 1, usage: {}, durationMs: 1, denials: [] });
  await quiet(() => runEval(o, { run, judge: okJudge, casesDir, check: none }));
  const [e] = readResults(o.out);
  assert.equal(calls, 2);
  assert.equal(e.error, undefined);
  assert.equal(e.cost, 1.5);
  // two failures are recorded, not thrown, and nothing is judged
  const b = setup((shas) => [kase(shas)]);
  let n = 0;
  await quiet(() => runEval(b.o, { run: async () => (n++, { code: 1, isError: true, text: "", cost: 0.25, stderr: "boom" }), judge: okJudge, casesDir: b.casesDir, check: none }));
  const [f] = readResults(b.o.out);
  assert.equal(n, 2);
  assert.match(f.error, /claude exited 1/);
  assert.equal(f.cost, 0.5);
  assert.equal(f.judge, undefined);
});

test("the judge is retried once on unusable output, and gives up after two", async () => {
  const good = JSON.stringify({ per_finding: [{ index: 0, verdict: "caught", evidence: "e" }], other_medium_plus: 1 });
  const seq = (...texts) => {
    let i = 0;
    const calls = [];
    const run = async (a) => (calls.push(a.args), { code: 0, isError: false, text: texts[i++], cost: 0.1 });
    return { run, calls };
  };
  const f = [{ severity: "Medium", summary: "s" }];
  const a = seq("not json", good);
  const r = await judgeReview({ findings: f, reviewText: "r", run: a.run });
  assert.equal(r.ok, true);
  assert.equal(a.calls.length, 2);
  assert.ok(Math.abs(r.cost - 0.2) < 1e-9);
  assert.deepEqual(a.calls[0].slice(a.calls[0].indexOf("--setting-sources"), a.calls[0].indexOf("--setting-sources") + 2), ["--setting-sources", ""]);
  assert.ok(a.calls[0].includes("--strict-mcp-config"));
  const b = seq("nope", '{"per_finding":[]}');
  const r2 = await judgeReview({ findings: f, reviewText: "r", run: b.run });
  assert.equal(r2.ok, false);
  assert.equal(b.calls.length, 2);
  const c = seq("x", "y");
  c.run = async () => ({ code: 1, isError: true, text: "", cost: 0.1, stderr: "down" });
  assert.match((await judgeReview({ findings: f, reviewText: "r", run: c.run })).error, /judge exited 1/);
});

test("--lens with no --cases runs only the class A and B cases that have findings", async () => {
  const { casesDir, o } = setup((shas) => {
    const k = (id, cls) => ({ ...kase(shas), id, findings: [{ summary: "x".repeat(50), severity: "Medium", class: cls, source: "s" }] });
    return [k("rtest-a", "A"), k("rtest-b", "B"), k("rtest-e", "E"), { ...kase(shas), id: "rtest-clean", kind: "clean", findings: [] }];
  });
  const seen = [];
  const run = async ({ input }) => (seen.push(input.match(/Lens for this run: .*/)[0]), { code: 0, isError: false, text: "Review: pass", cost: 0, usage: {}, durationMs: 1, denials: [] });
  await quiet(() => runEval({ ...o, lens: "security" }, { run, judge: okJudge, casesDir, check: none }));
  assert.deepEqual(readResults(o.out).map((e) => e.case).sort(), ["rtest-a", "rtest-b"]);
  assert.equal(seen.length, 2);
  assert.ok(seen.every((l) => l === "Lens for this run: look hardest at security boundaries."));
  // with --cases the selection is the user's
  const b = setup((shas) => [{ ...kase(shas), id: "rtest-e", findings: [{ summary: "x".repeat(50), severity: "Medium", class: "E", source: "s" }] }]);
  await quiet(() => runEval({ ...b.o, lens: "security", cases: "rtest-e" }, { run, judge: okJudge, casesDir: b.casesDir, check: none }));
  assert.deepEqual(readResults(b.o.out).map((e) => e.case), ["rtest-e"]);
});

test("stale scratch checkouts are cleared, live ones kept", () => {
  const work = tmp();
  for (const d of ["case-1-999999999", `case-2-${process.pid}`, "no-pid-here"]) fs.mkdirSync(path.join(work, d));
  clearStaleWork(work);
  assert.deepEqual(fs.readdirSync(work), [`case-2-${process.pid}`]);
  clearStaleWork(path.join(work, "missing"));
});

test("the issue check names every case gh can't read", () => {
  const cases = [{ id: "a", repo: "tlockney/x", issue: 1 }, { id: "b", repo: "tlockney/x", issue: 2 }];
  assert.doesNotThrow(() => checkIssues(cases, () => {}));
  assert.throws(() => checkIssues(cases, (c) => { if (c.id === "b") throw Object.assign(new Error("x"), { stderr: "HTTP 401" }); }), (e) => /b: gh api repos\/tlockney\/x\/issues\/2/.test(e.message) && /HTTP 401/.test(e.message) && !/a: gh/.test(e.message));
});
