// node --test evals/review/*.test.mjs
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { assemblePrompt, lensLine, loadCases, selectCases, validateCase } from "./cases.mjs";
import { ALLOWED, DISALLOWED, parseArgs } from "./run.mjs";

const dir = path.join(path.dirname(fileURLToPath(import.meta.url)), "cases");
const sha = (c) => c.repeat(40);
const good = () => ({
  id: "ganymede-1", repo: "tlockney/ganymede", pr: 1, issue: 2, base: sha("a"), head: sha("b"), kind: "known", notes: "",
  findings: [{ summary: "x".repeat(50), severity: "Medium", class: "A", source: "https://example.test" }],
});

test("every case file in the repo is valid", () => {
  const cases = loadCases(dir);
  assert.ok(cases.length >= 20);
  const kinds = new Set(cases.map((c) => c.kind));
  assert.deepEqual([...kinds].sort(), ["clean", "known", "missed"]);
  assert.ok(cases.filter((c) => c.kind === "clean").length >= 3);
});

test("validation names each problem", () => {
  assert.deepEqual(validateCase(good(), "ganymede-1.json"), []);
  const bad = { ...good(), head: "abc1234", kind: "weird", findings: [{ summary: "short", severity: "Low", class: "Z" }] };
  const p = validateCase(bad).join("\n");
  assert.match(p, /head must be a full/);
  assert.match(p, /kind must be/);
  assert.match(p, /summary must describe/);
  assert.match(p, /severity/);
  assert.match(p, /class must be/);
  assert.match(p, /source/);
  assert.match(validateCase(good(), "other.json").join(), /must match the file name/);
  assert.match(validateCase({ ...good(), findings: [] }).join(), /at least one finding/);
  assert.match(validateCase({ ...good(), kind: "clean" }).join(), /no findings/);
  assert.deepEqual(validateCase({ ...good(), kind: "clean", findings: [] }), []);
});

test("selecting cases by id and wildcard", () => {
  const cases = loadCases(dir);
  assert.deepEqual(selectCases(cases, "ganymede-728").map((c) => c.id), ["ganymede-728"]);
  assert.ok(selectCases(cases, "cdp-kernel-*").length === 4);
  assert.equal(selectCases(cases, null).length, cases.length);
  assert.throws(() => selectCases(cases, "nope"), /no case matches/);
});

test("the prompt is the file word for word, then only the trailer", () => {
  const file = "# Prompt\n\nLine one.\n  Indented, with `code` and $1 and \\n.\n";
  const t = { repo: "tlockney/ganymede", clone: "/w/x", branch: "b/1", issue: 7, diff: "diff --git a/f b/f\n+x\n" };
  const got = assemblePrompt(file, t);
  assert.ok(got.startsWith(file), "the prompt text comes first, unchanged");
  assert.equal(got.slice(file.length), "\nREPO: tlockney/ganymede (local clone: /w/x)\nBRANCH: b/1\nISSUE: 7\nDIFF (git diff origin/main...HEAD):\ndiff --git a/f b/f\n+x\n");
  const lens = assemblePrompt(file, { ...t, lens: "security" });
  assert.equal(lens.slice(file.length), `\n${lensLine("security")}\n` + got.slice(file.length));
  assert.equal(lensLine("concurrency"), "Lens for this run: look hardest at concurrency and lifecycle.");
  assert.throws(() => lensLine("style"), /unknown lens/);
});

test("the real review prompt is passed whole", () => {
  const p = "/mnt/project-files/reviews/pr-reviewer/review-prompt.md";
  if (!fs.existsSync(p)) return;
  const file = fs.readFileSync(p, "utf8");
  const got = assemblePrompt(file, { repo: "r", clone: "/c", branch: "b", issue: 1, diff: "d" });
  assert.ok(got.startsWith(file));
});

test("arguments", () => {
  assert.throws(() => parseArgs([]), /--prompt/);
  const o = parseArgs(["--prompt", "p.md"]);
  assert.equal(o.runs, 2);
  assert.equal(o.model, "opus");
  assert.equal(o.judgeModel, "sonnet");
  assert.equal(o.concurrency, 2);
  assert.equal(parseArgs(["--prompt", "p", "--lens", "security"]).runs, 1);
  assert.throws(() => parseArgs(["--prompt", "p", "--lens", "x"]), /unknown lens/);
  assert.throws(() => parseArgs(["--prompt", "p", "--wat"]), /unknown argument/);
});

test("reviews are read-only", () => {
  for (const w of ["Edit", "Write", "NotebookEdit"]) assert.ok(DISALLOWED.split(",").includes(w));
  assert.ok(!/Bash\((?:rm|mv|cp|git push|git commit|git checkout|cargo|npm)/.test(ALLOWED));
  assert.ok(!ALLOWED.split(",").some((t) => ["Edit", "Write", "Bash"].includes(t)));
});
