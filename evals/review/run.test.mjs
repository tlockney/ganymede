// node --test evals/review/*.test.mjs
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { checkout, runEval } from "./run.mjs";
import { readResults } from "./store.mjs";

const git = (dir, ...a) => execFileSync("git", ["-C", dir, ...a], { encoding: "utf8" }).trim();
const tmp = () => fs.mkdtempSync(path.join(os.tmpdir(), "review-run-"));

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
  const log = console.error;
  console.error = () => {};
  try {
    await runEval(o, { run: stubRun, judge: failingJudge, casesDir });
    let [e] = readResults(out);
    assert.match(e.error, /judge: garbled/);
    assert.equal(e.review, path.join("reviews", "rtest-1-1.md"));
    assert.equal(reviews, 1);
    await runEval(o, { run: stubRun, judge: goodJudge, casesDir });
    [e] = readResults(out);
    assert.equal(reviews, 1, "the saved review was not run again");
    assert.equal(e.error, undefined);
    assert.equal(e.judge.perFinding[0].verdict, "caught");
    assert.equal(e.cost, 1, "the review's cost is kept");
    assert.equal(e.denials, 2);
    assert.equal(e.verdict, "pass");
    await runEval(o, { run: stubRun, judge: goodJudge, casesDir });
    assert.equal(judges, 2, "a judged run is left alone");
    // another prompt is refused; --redo with it runs the selection again
    fs.writeFileSync(prompt, "# Another prompt\n");
    await assert.rejects(runEval(o, { run: stubRun, judge: goodJudge, casesDir }), /another prompt or model/);
    await runEval({ ...o, redo: true }, { run: stubRun, judge: goodJudge, casesDir });
    assert.equal(reviews, 2);
    assert.equal(readResults(out).length, 1);
    assert.ok(fs.existsSync(path.join(out, "report.md")));
  } finally {
    console.error = log;
  }
});
