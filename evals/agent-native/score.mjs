// Scoring for the person-and-agent evals (#824): pure functions over the
// finished .ipynb and the person's log (person.json: which steps ran, and
// when), so tests can feed them by hand. No model judges anything.

import { agentOf, cellText } from "../lib/notebook.mjs";

export { agentOf, cellText };

/** `metadata.ganymede.request`, set when an agent answered the cell. */
export function requestOf(cell) {
  const r = cell.metadata?.ganymede?.request;
  return r && typeof r === "object" ? r : null;
}

/** Whether a request cell is marked done with an answer. */
export function isAnswered(cell) {
  const r = requestOf(cell);
  return Boolean(r && r.done && r.answer && r.id === cell.id);
}

export function byId(nb, id) {
  return nb.cells.find((c) => c.id === id);
}

/**
 * One run's score: the scenario's check on the notebook, and that the
 * person got to do every step (a step whose condition never held means
 * the interleaving the scenario tests didn't happen, so it fails).
 * `person` is person.py's log, or null.
 */
export function score(scenario, nb, person) {
  const { pass, reasons } = scenario.check(nb, person);
  const out = [...reasons];
  const steps = person?.steps ?? [];
  if (!person) out.push("no log from the simulated person");
  else if (steps.length < scenario.person.length || steps.some((s) => s.status !== "done")) {
    const bad = steps.find((s) => s.status !== "done");
    out.push(
      bad
        ? `the person's step ${bad.step} ${bad.status === "timeout" ? `never saw ${JSON.stringify(bad.wait)}` : `failed: ${bad.error}`}`
        : `the person did ${steps.length} of ${scenario.person.length} steps`,
    );
  }
  return { pass: pass && out.length === 0, reasons: out };
}

/** Per-scenario counts over scored runs; runs with no score are left out. */
export function summarize(results) {
  const by = {};
  for (const r of results) {
    const s = (by[r.scenario] ??= { runs: 0, passed: 0, excluded: 0, failures: [] });
    if (!r.score) {
      s.excluded++;
      continue;
    }
    s.runs++;
    if (r.score.pass) s.passed++;
    else s.failures.push({ run: r.run, reasons: r.score.reasons });
  }
  return by;
}

/** report.md text for a set of results. */
export function reportMarkdown(results) {
  const by = summarize(results);
  const lines = ["# Person and agent eval", "", "| Scenario | Passed | Excluded |", "| --- | --- | --- |"];
  let runs = 0;
  let passed = 0;
  for (const [id, s] of Object.entries(by)) {
    lines.push(`| ${id} | ${s.passed}/${s.runs} | ${s.excluded} |`);
    runs += s.runs;
    passed += s.passed;
  }
  lines.push("", `Overall: ${passed}/${runs} scored runs passed.`, "");
  for (const r of results.filter((r) => !r.score)) lines.push(`- excluded ${r.run}: ${r.error}`);
  for (const [id, s] of Object.entries(by)) {
    for (const f of s.failures) {
      lines.push(`- failed ${f.run} (${id}): ${f.reasons.join("; ")}`);
    }
  }
  return lines.join("\n") + "\n";
}

/** The prefixes of the ganymede MCP tools' names, as the plugin or `claude mcp add` names them. */
const TOOL_PREFIXES = ["mcp__plugin_ganymede-journal_ganymede__", "mcp__ganymede__"];

/**
 * How many ganymede MCP tools the agent called, read from a session's
 * `--output-format stream-json` log. 0 means the agent never reached the
 * server (or the log isn't stream-json), so what the notebook shows says
 * nothing about it: the run is left out rather than scored.
 */
export function ganymedeToolCalls(log) {
  let n = 0;
  for (const line of String(log).split("\n")) {
    if (!line.startsWith("{")) continue;
    let ev;
    try {
      ev = JSON.parse(line);
    } catch {
      continue;
    }
    if (ev.type !== "assistant") continue;
    for (const b of ev.message?.content ?? []) {
      if (b.type === "tool_use" && TOOL_PREFIXES.some((p) => String(b.name).startsWith(p))) n++;
    }
  }
  return n;
}

/** Why a run can't be scored from its agent log, or null. */
export function noEvidence(log) {
  return ganymedeToolCalls(log) > 0 ? null : "the agent made no ganymede tool call (or its log isn't stream-json)";
}
