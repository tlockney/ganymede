// node --test evals/review/*.test.mjs
import assert from "node:assert/strict";
import test from "node:test";
import { aggregate, buildJudgePrompt, extractJsonObject, lensComparison, lensSentence, parseJudge, recallOf, renderReport, reviewVerdict } from "./score.mjs";

const f = (cls, severity = "Medium") => ({ summary: "s".repeat(50), severity, class: cls, source: "x" });
const cases = [
  { id: "k1", kind: "known", findings: [f("A"), f("B", "High")] },
  { id: "m1", kind: "missed", findings: [f("A")] },
  { id: "k2", kind: "known", findings: [f("E")] },
  { id: "c1", kind: "clean", findings: [] },
];
const run = (id, n, verdicts, other = 0, extra = {}) => ({
  case: id, lens: null, run: n, verdict: other ? "changes needed" : "pass", cost: 1,
  judge: { perFinding: verdicts.map((verdict) => ({ verdict, evidence: "" })), otherMediumPlus: other }, ...extra,
});

test("judge output is found in fences, prose and nested braces", () => {
  const body = { per_finding: [{ index: 0, verdict: "caught", evidence: "a } brace and \"quote\"" }], other_medium_plus: 2 };
  const text = JSON.stringify(body);
  for (const wrapped of [text, "```json\n" + text + "\n```", `Here you go: ${text}\nHope that helps {not json}`, "{oops} " + text]) {
    const p = parseJudge(wrapped, 1);
    assert.equal(p.ok, true, wrapped);
    assert.deepEqual(p.perFinding, [{ verdict: "caught", evidence: 'a } brace and "quote"' }]);
    assert.equal(p.otherMediumPlus, 2);
  }
  assert.deepEqual(extractJsonObject("no json"), null);
});

test("judge output is parsed defensively", () => {
  const ok = (o, n) => parseJudge(JSON.stringify(o), n);
  assert.equal(parseJudge("I cannot do that", 1).ok, false);
  assert.match(ok({ per_finding: [] }, 1).error, /no verdict for finding 0/);
  assert.match(ok({ per_finding: [{ index: 0, verdict: "great" }] }, 1).error, /unknown verdict/);
  assert.match(ok({ other_medium_plus: 1 }, 0).error, /per_finding/);
  assert.match(ok({ per_finding: [], other_medium_plus: -1 }, 0).error, /count/);
  // partial and capitals are normalized; an empty list is fine for a clean case.
  assert.equal(ok({ per_finding: [{ index: 0, verdict: "Partial" }] }, 1).perFinding[0].verdict, "partly");
  assert.deepEqual(ok({ per_finding: [] }, 0), { ok: true, perFinding: [], otherMediumPlus: 0 });
  // entries out of order are placed by index; missing index falls back to order
  const p = ok({ per_finding: [{ index: 1, verdict: "missed" }, { index: 0, verdict: "caught" }], other_medium_plus: 0 }, 2);
  assert.deepEqual(p.perFinding.map((x) => x.verdict), ["caught", "missed"]);
  assert.deepEqual(ok({ per_finding: [{ verdict: "caught" }, { verdict: "missed" }] }, 2).perFinding.map((x) => x.verdict), ["caught", "missed"]);
});

test("the judge prompt lists the findings by index and asks for JSON", () => {
  const p = buildJudgePrompt([f("A"), f("B", "High")], "REVIEW BODY");
  assert.match(p, /\[0\] \(Medium\)/);
  assert.match(p, /\[1\] \(High\)/);
  assert.match(p, /REVIEW BODY/);
  assert.match(p, /other_medium_plus/);
  assert.match(buildJudgePrompt([], "r"), /none: this change is known to need no fix/);
});

test("the review's verdict is its last Review line", () => {
  assert.equal(reviewVerdict("a\nReview: pass\n"), "pass");
  assert.equal(reviewVerdict("Review: pass in the title\n...\n**Review: changes needed**"), "changes needed");
  assert.equal(reviewVerdict("nothing"), null);
});

test("recall counts partly as half, single-run over every run and any-run over the best", () => {
  const rows = [{ pts: [1, 0] }, { pts: [0.5, 0.5] }, { pts: [0, 0] }];
  const r = recallOf(rows);
  assert.equal(r.findings, 3);
  assert.ok(Math.abs(r.single - 2 / 6) < 1e-9);
  assert.ok(Math.abs(r.any - 1.5 / 3) < 1e-9);
  assert.equal(r.caughtAny, 1);
});

test("aggregate: recall overall, by class and kind; false alarms from clean cases only", () => {
  const entries = [
    run("k1", 1, ["caught", "missed"], 1), run("k1", 2, ["partly", "caught"], 0),
    run("m1", 1, ["missed"]), run("m1", 2, ["caught"]),
    run("k2", 1, ["missed"]), run("k2", 2, ["missed"]),
    run("c1", 1, [], 2), run("c1", 2, [], 0),
  ];
  const a = aggregate(entries, cases);
  // findings: k1/A runs 1,.5; k1/B runs 0,1; m1/A runs 0,1; k2/E 0,0 -> sum 3.5 over 8
  assert.equal(a.recall.findings, 4);
  assert.equal(a.recall.runs, 8);
  assert.ok(Math.abs(a.recall.single - 3.5 / 8) < 1e-9);
  assert.ok(Math.abs(a.recall.any - (1 + 1 + 1 + 0) / 4) < 1e-9);
  assert.equal(a.byClass.A.findings, 2);
  assert.ok(Math.abs(a.byClass.A.single - 2.5 / 4) < 1e-9);
  assert.ok(Math.abs(a.byClass.E.single) < 1e-9);
  assert.ok(Math.abs(a.byKind.missed.single - 0.5) < 1e-9);
  assert.equal(a.falseAlarms.total, 2);
  assert.equal(a.falseAlarms.perRun, 1);
  assert.equal(a.falseAlarms.runsWithAny, 1);
  assert.equal(a.falseAlarms.runsChangesNeeded, 1);
  assert.equal(a.otherOnFindingCases.runs, 6);
  assert.ok(Math.abs(a.otherOnFindingCases.perRun - 1 / 6) < 1e-9);
  assert.equal(a.cost.reviews, 8);
});

test("aggregate leaves out failed runs and lens runs, and lists failures", () => {
  const entries = [
    run("k2", 1, ["caught"]),
    { case: "k2", lens: null, run: 2, error: "claude exited 1", judge: null },
    run("k2", 3, ["missed"], 0, { lens: "security" }),
    { case: "c1", lens: null, run: 1, review: "r", judge: null },
  ];
  const a = aggregate(entries, cases);
  assert.equal(a.recall.runs, 1);
  assert.equal(a.recall.single, 1);
  assert.equal(a.falseAlarms.cleanRuns, 0);
  assert.equal(a.falseAlarms.perRun, null);
  assert.deepEqual(a.failures.map((x) => x.error), ["claude exited 1", "not judged"]);
});

test("lens comparison: one run, two plain runs, security plus concurrency", () => {
  const lens = (id, name, verdicts) => run(id, 1, verdicts, 0, { lens: name });
  const entries = [
    // k1: A and B findings. plain runs miss B; lenses catch both between them.
    run("k1", 1, ["caught", "missed"]), run("k1", 2, ["missed", "missed"]),
    lens("k1", "security", ["missed", "caught"]), lens("k1", "concurrency", ["caught", "missed"]),
    // k2 is class E only: not in the comparison.
    run("k2", 1, ["caught"]), run("k2", 2, ["caught"]), lens("k2", "security", ["caught"]), lens("k2", "concurrency", ["caught"]),
    // m1 lacks a concurrency run: left out.
    run("m1", 1, ["caught"]), run("m1", 2, ["caught"]), lens("m1", "security", ["caught"]),
  ];
  const l = lensComparison(entries, cases);
  assert.equal(l.cases, 1);
  assert.equal(l.findings, 2);
  assert.ok(Math.abs(l.onePlainRun - 0.25) < 1e-9);
  assert.ok(Math.abs(l.plainPair - 0.5) < 1e-9);
  assert.equal(l.lensPair, 1);
  assert.equal(l.conclusion, "justified");
  assert.equal(l.cost.lensPair, 2);
  assert.equal(l.cost.plainPair, 2);
  assert.match(lensSentence(l), /one plain run catches 25%, two plain runs 50%, .* 100%/);
  // A lens pair no better than one run is not justified.
  const flat = [run("k1", 1, ["caught", "caught"]), run("k1", 2, ["caught", "caught"]), lens("k1", "security", ["caught", "caught"]), lens("k1", "concurrency", ["caught", "caught"])];
  assert.equal(lensComparison(flat, cases).conclusion, "not justified");
  assert.equal(lensComparison([], cases).findings, 0);
  assert.match(lensSentence(lensComparison([], cases)), /Not measured/);
});

test("the report shows the measures", () => {
  const entries = [run("k1", 1, ["caught", "missed"], 1), run("k1", 2, ["partly", "caught"]), run("c1", 1, [], 1), run("c1", 2, [])];
  const summary = { agg: aggregate(entries, cases), lens: lensComparison(entries, cases) };
  const md = renderReport(summary, entries, cases, { date: "2026-11-14", prompt: "p.md", promptSha: "a".repeat(64), model: "opus", judgeModel: "sonnet", runs: 2 });
  assert.match(md, /## Recall/);
  assert.match(md, /class A, Concurrency/);
  assert.match(md, /0\.50 other Medium\+ findings per run/);
  assert.match(md, /## Two lenses vs one run/);
  assert.match(md, /Not measured/);
});
