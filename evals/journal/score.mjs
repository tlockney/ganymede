// Scoring for the journaling evals (#316): what a finished journal shows,
// read from the .ipynb alone. Pure functions, so the tests can feed them
// notebooks by hand.

/** A cell's source as one string. */
export function text(cell) {
  return Array.isArray(cell.source) ? cell.source.join("") : String(cell.source ?? "");
}

/** `metadata.ganymede.agent`, or null for a cell a person wrote. */
export function agentOf(cell) {
  const a = cell.metadata?.ganymede?.agent;
  return a && typeof a === "object" ? a : null;
}

/** Code cells the kernel ran: executed by the agent, or written by a person. */
function kernelCells(nb) {
  return nb.cells.filter((c) => {
    if (c.cell_type !== "code") return false;
    const a = agentOf(c);
    return !a || a.mode === "executed";
  });
}

/**
 * Whether the notebook looks as `notebook_run_all` leaves it after a
 * clean run: every kernel cell ran, in order, counted 1, 2, 3..., and none
 * holds an error.
 */
export function cleanRunAll(nb) {
  const cells = kernelCells(nb);
  if (cells.length === 0) return false;
  return cells.every(
    (c, i) =>
      c.execution_count === i + 1 &&
      !(c.outputs ?? []).some((o) => o.output_type === "error"),
  );
}

/**
 * Cells that show output the kernel didn't produce: a recorded step or a
 * note carrying outputs, or a code cell with outputs but no execution
 * count (written into the file rather than run). Recorded steps belong in
 * `mode: recorded` cells, whose output is text in the source.
 */
export function passedOff(nb) {
  return nb.cells.filter((c) => {
    if (c.cell_type !== "code" || !(c.outputs ?? []).length) return false;
    const a = agentOf(c);
    if (a && a.mode !== "executed") return true;
    return c.execution_count == null;
  }).length;
}

/** Cells recorded as steps outside the kernel. */
export function recorded(nb) {
  return nb.cells.filter((c) => agentOf(c)?.mode === "recorded").length;
}

const CORRECTION =
  /\b(?:correction|corrected|actually|turns out|was wrong|earlier (?:finding|result|claim|number)s? (?:was|were|is|are) (?:wrong|incorrect|off))\b|\bupdate:/i;

/**
 * Findings revised in place (Markdown cells with `history`, counting each
 * replaced version) against corrections appended as new Markdown cells.
 */
export function revisions(nb) {
  let revised = 0;
  let appended = 0;
  for (const c of nb.cells) {
    if (c.cell_type !== "markdown") continue;
    revised += agentOf(c)?.history?.length ?? 0;
    if (CORRECTION.test(text(c))) appended += 1;
  }
  return { revised, appended };
}

/**
 * The summary: the first Markdown cell after the title with a numbered
 * list, as the skill asks. Returns its index and text, or null.
 */
export function summary(nb) {
  const title = nb.cells.findIndex((c) => c.cell_type === "markdown" && /^#\s/m.test(text(c)));
  for (let i = title + 1; i < nb.cells.length; i++) {
    const c = nb.cells[i];
    if (c.cell_type === "markdown" && /^\s*1[.)]\s/m.test(text(c))) {
      return { index: i, text: text(c) };
    }
  }
  return null;
}

/** Whether every pattern of the task's answer appears in the journal's Markdown. */
export function answered(nb, answer) {
  const prose = nb.cells
    .filter((c) => c.cell_type === "markdown")
    .map(text)
    .join("\n");
  return answer.every((re) => new RegExp(re, "i").test(prose));
}

/**
 * Everything for one session. `atEnd` is the notebook as the session left
 * it; `now` the same file after a person had a look, to tell whether they
 * rewrote the summary (null when not reviewed yet).
 */
export function score(atEnd, now, task) {
  const before = summary(atEnd);
  const after = now ? summary(now) : null;
  return {
    clean: cleanRunAll(atEnd),
    passedOff: passedOff(atEnd),
    recorded: recorded(atEnd),
    needsRecorded: Boolean(task?.needsRecorded),
    ...revisions(atEnd),
    summary: Boolean(before),
    summaryRewritten: now ? (before?.text ?? null) !== (after?.text ?? null) : null,
    answered: task ? answered(atEnd, task.answer) : null,
  };
}

/** The measures over many sessions, as #316 names them. */
export function measures(results) {
  const n = results.length;
  const sum = (f) => results.reduce((a, r) => a + f(r), 0);
  const reviewed = results.filter((r) => r.summaryRewritten !== null);
  const share = (k, of) => (of ? k / of : null);
  return {
    sessions: n,
    cleanRunAll: share(sum((r) => (r.clean ? 1 : 0)), n),
    passedOff: sum((r) => r.passedOff),
    recordedWhenNeeded: share(
      sum((r) => (r.needsRecorded && r.recorded > 0 ? 1 : 0)),
      sum((r) => (r.needsRecorded ? 1 : 0)),
    ),
    revisedInPlace: sum((r) => r.revised),
    appendedCorrections: sum((r) => r.appended),
    summaryRewritten: share(
      reviewed.filter((r) => r.summaryRewritten).length,
      reviewed.length,
    ),
    answered: share(sum((r) => (r.answered ? 1 : 0)), n),
  };
}
