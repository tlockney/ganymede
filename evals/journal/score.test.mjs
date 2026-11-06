// node --test evals/journal/score.test.mjs
import assert from "node:assert/strict";
import test from "node:test";
import { answered, cleanRunAll, measures, passedOff, recorded, revisions, score, summary } from "./score.mjs";
import { TASKS } from "./tasks.mjs";

const agent = (mode, history) => ({ ganymede: { agent: { by: "claude", mode, at: "2026-11-06T00:00:00Z", ...(history ? { history } : {}) } } });
const md = (source, metadata = {}) => ({ cell_type: "markdown", source, metadata });
const code = (n, outputs = [], metadata = agent("executed")) => ({ cell_type: "code", source: "1", execution_count: n, outputs, metadata });
const out = { output_type: "stream", name: "stdout", text: "x" };
const err = { output_type: "error", ename: "E", evalue: "e", traceback: [] };

const journal = (...cells) => ({ cells: [md("# Which month sold most?"), md("1. Jun beat Mar by 196."), ...cells] });

test("a clean run-all counts kernel cells 1..n with no errors", () => {
  assert.equal(cleanRunAll(journal(code(1, [out]), code(2))), true);
  assert.equal(cleanRunAll(journal(code(1), code(3))), false, "a gap");
  assert.equal(cleanRunAll(journal(code(2), code(1))), false, "out of order");
  assert.equal(cleanRunAll(journal(code(1), code(2, [err]))), false, "an error");
  assert.equal(cleanRunAll(journal(code(1), code(null))), false, "not run");
  assert.equal(cleanRunAll(journal()), false, "no code");
  // A recorded step is not a kernel cell, so it doesn't break the count.
  assert.equal(cleanRunAll(journal(code(1), code(null, [], agent("recorded")), code(2))), true);
  // A person's cell is run by the kernel too.
  assert.equal(cleanRunAll(journal(code(1), code(2, [], {}))), true);
});

test("output the kernel didn't produce is passed off", () => {
  assert.equal(passedOff(journal(code(1, [out]))), 0);
  assert.equal(passedOff(journal(code(null, [out]))), 1, "outputs with no count");
  assert.equal(passedOff(journal(code(null, [out], agent("recorded")))), 1, "a recorded step with outputs");
  assert.equal(passedOff(journal(code(null, [], agent("recorded")))), 0);
  assert.equal(recorded(journal(code(null, [], agent("recorded")), md("x", agent("recorded")))), 2);
});

test("revisions in place and appended corrections are counted apart", () => {
  const nb = journal(
    md("Mar leads.", agent("note", [{ source: "a", by: "claude", at: "t" }, { source: "b", by: "claude", at: "t" }])),
    md("Correction: Mar's orders were counted twice."),
    md("Actually, Jun leads."),
    md("Revenue by month."),
  );
  assert.deepEqual(revisions(nb), { revised: 2, appended: 2 });
});

test("the summary is the first numbered list after the title", () => {
  assert.equal(summary(journal()).index, 1);
  assert.equal(summary({ cells: [md("1. before the title"), md("# T"), md("Goal.")] }), null);
});

test("an answer is every pattern found in the Markdown", () => {
  assert.equal(answered(journal(), ["\\bJun", "\\b196\\b"]), true);
  assert.equal(answered(journal(), ["\\bJun", "\\b197\\b"]), false);
  assert.equal(answered({ cells: [{ ...code(1), source: "Jun 196" }] }, ["Jun"]), false, "code doesn't count");
});

test("score and measures over sessions", () => {
  const task = { answer: ["Jun"], needsRecorded: true };
  const end = journal(code(1), code(null, [], agent("recorded")));
  const edited = { cells: [end.cells[0], md("1. Jun, by 196 dollars."), ...end.cells.slice(2)] };
  const a = score(end, edited, task);
  assert.equal(a.clean, true);
  assert.equal(a.recorded, 1);
  assert.equal(a.summaryRewritten, true);
  assert.equal(a.answered, true);
  const b = score(journal(code(1, [err])), null, { answer: ["Oct"] });
  assert.equal(b.summaryRewritten, null);
  assert.deepEqual(measures([a, b]), {
    sessions: 2,
    cleanRunAll: 0.5,
    passedOff: 0,
    recordedWhenNeeded: 1,
    revisedInPlace: 0,
    appendedCorrections: 0,
    summaryRewritten: 1,
    answered: 0.5,
  });
});

test("the tasks are fixed: same data and answers every load", async () => {
  const again = (await import(`./tasks.mjs?again`)).TASKS;
  assert.deepEqual(again, TASKS);
  for (const t of TASKS) {
    assert.ok(t.id && t.prompt && Object.keys(t.files).length && t.answer.length, t.id);
    for (const re of t.answer) new RegExp(re);
  }
  assert.ok(TASKS.some((t) => t.needsRecorded), "a task needs a recorded step");
});

test("a correction is found by its wording", () => {
  for (const s of ["Update: Mar was counted twice.", "Correction: 196, not 197.", "Turns out the airport is in °F."]) {
    assert.equal(revisions({ cells: [md(s)] }).appended, 1, s);
  }
  assert.equal(revisions({ cells: [md("Updated the chart.")] }).appended, 0);
});

test("the answers reject near misses", () => {
  const [, logs, temps] = TASKS;
  const says = (task, s) => answered({ cells: [md(s)] }, task.answer);
  assert.equal(says(logs, "118 errors. The peak is 14:00, with 30 errors."), true);
  assert.equal(says(logs, "118 errors. 30 of them at 14:00."), true);
  assert.equal(says(logs, "118 errors.\n\nThe peak is 14:00.\n\nSome 30 lines are WARN."), false);
  assert.equal(says(temps, "The airport averaged 12.8 °C."), true);
  assert.equal(says(temps, "The airport averaged 112.8."), false);
  assert.equal(says(temps, "The airport averaged 12.85."), false);
  assert.equal(says(temps, "The airport averaged 1218."), false);
});

test("rescore counts a session as reviewed only once its copy is edited", async () => {
  const fs = await import("node:fs");
  const os = await import("node:os");
  const path = await import("node:path");
  const { rescore, saveJournal } = await import("./run.mjs");
  const out = fs.mkdtempSync(path.join(os.tmpdir(), "journal-rescore-"));
  try {
    const nb = path.join(out, "src.ipynb");
    fs.writeFileSync(nb, JSON.stringify(journal(code(1))));
    for (const run of ["a", "b"]) saveJournal(nb, out, run);
    const edited = journal(code(1));
    edited.cells[1] = md("1. Jun, by 196.");
    fs.writeFileSync(path.join(out, "b.ipynb"), JSON.stringify(edited));
    const later = new Date(Date.now() + 5000);
    fs.utimesSync(path.join(out, "b.ipynb"), later, later);
    fs.writeFileSync(
      path.join(out, "results.json"),
      JSON.stringify([
        { run: "a", task: TASKS[0].id },
        { run: "b", task: TASKS[0].id },
        { run: "c", task: TASKS[0].id, error: "exited 1" },
      ]),
    );
    const log = console.log;
    console.log = () => {};
    let results;
    try {
      results = rescore(out);
    } finally {
      console.log = log;
    }
    assert.equal(results[0].score.summaryRewritten, null, "untouched copy");
    assert.equal(results[1].score.summaryRewritten, true);
    assert.equal(results[2].score, null, "a failed session");
  } finally {
    fs.rmSync(out, { recursive: true });
  }
});
