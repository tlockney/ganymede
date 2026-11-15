// Judge prompt, judge-output parsing and the measures. Pure: no model
// calls, no git, no files.

import { CLASSES } from "./cases.mjs";

export const VERDICTS = ["caught", "partly", "missed"];
export const POINTS = { caught: 1, partly: 0.5, missed: 0 };

/** What the Sonnet judge is asked, for one review and the case's known findings. */
export function buildJudgePrompt(findings, reviewText) {
  const known = findings.length
    ? findings.map((f, i) => `[${i}] (${f.severity}) ${f.summary}`).join("\n\n")
    : "(none: this change is known to need no fix)";
  return `You score one code review against known findings. Do not review the code yourself and do not check whether findings are true; only compare the review's text with the list.

KNOWN FINDINGS:
${known}

REVIEW TEXT:
<review>
${reviewText}
</review>

For each known finding give a verdict:
- "caught": the review reports the same defect (same mechanism, same place or an equivalent one) as a finding at Medium or High severity, or as a blocking item, so that acting on the review would fix it.
- "partly": the review reports only part of it, a nearby or weaker version, the right area without the failing scenario, or the same defect but only as a Low, nit or "plausible/unconfirmed" remark.
- "missed": the review does not mention it.

Also count "other_medium_plus": the findings the review itself rates Medium or High (or lists as blocking) that are NOT one of the known findings above. Count each distinct finding once. Do not count Low, nit or info items.

Reply with one JSON object and nothing else:
{"per_finding":[{"index":0,"verdict":"caught|partly|missed","evidence":"short quote or reason"}],"other_medium_plus":0}
Give one entry per known finding (none for an empty list).`;
}

/** The first balanced `{...}` in text, respecting strings; null when none. */
export function extractJsonObject(text) {
  for (let start = text.indexOf("{"); start !== -1; start = text.indexOf("{", start + 1)) {
    let depth = 0;
    let inStr = false;
    let esc = false;
    for (let i = start; i < text.length; i++) {
      const ch = text[i];
      if (inStr) {
        if (esc) esc = false;
        else if (ch === "\\") esc = true;
        else if (ch === '"') inStr = false;
      } else if (ch === '"') inStr = true;
      else if (ch === "{") depth++;
      else if (ch === "}" && --depth === 0) {
        try {
          return JSON.parse(text.slice(start, i + 1));
        } catch {
          break;
        }
      }
    }
  }
  return null;
}

const normalizeVerdict = (v) => {
  const s = String(v ?? "").trim().toLowerCase();
  if (s === "caught" || s === "partly" || s === "missed") return s;
  if (s === "partial" || s === "partially") return "partly";
  if (s === "miss" || s === "not caught") return "missed";
  if (s === "catch" || s === "hit") return "caught";
  return null;
};

/**
 * Parses judge output for a case with `n` known findings. Returns
 * `{ ok: true, perFinding: [{verdict, evidence}] (length n), otherMediumPlus }`
 * or `{ ok: false, error }`. Entries are matched by `index`, else by order;
 * a missing entry or an unknown verdict makes the output unusable.
 */
export function parseJudge(text, n) {
  const obj = extractJsonObject(String(text ?? ""));
  if (!obj) return { ok: false, error: "no JSON object in judge output" };
  const list = obj.per_finding;
  if (!Array.isArray(list)) return { ok: false, error: "per_finding is not an array" };
  const out = new Array(n).fill(null);
  list.forEach((e, order) => {
    if (!e || typeof e !== "object") return;
    const idx = Number.isInteger(e.index) && e.index >= 0 && e.index < n ? e.index : order;
    if (idx < n && out[idx] === null) out[idx] = { verdict: normalizeVerdict(e.verdict), evidence: String(e.evidence ?? "") };
  });
  for (let i = 0; i < n; i++) {
    if (!out[i]) return { ok: false, error: `no verdict for finding ${i}` };
    if (!out[i].verdict) return { ok: false, error: `unknown verdict for finding ${i}` };
  }
  if (obj.other_medium_plus === undefined || obj.other_medium_plus === null) return { ok: false, error: "other_medium_plus is missing" };
  const other = Number(obj.other_medium_plus);
  if (!Number.isFinite(other) || other < 0) return { ok: false, error: "other_medium_plus is not a count" };
  return { ok: true, perFinding: out, otherMediumPlus: Math.round(other) };
}

/** The review's own verdict line: "pass", "changes needed", or null. */
export function reviewVerdict(text) {
  const m = [...String(text ?? "").matchAll(/^\W*Review:\s*(pass|changes needed)\b/gim)].pop();
  return m ? m[1].toLowerCase() : null;
}

const mean = (xs) => (xs.length ? xs.reduce((a, b) => a + b, 0) / xs.length : null);
const sum = (xs) => xs.reduce((a, b) => a + b, 0);

/**
 * A usable entry is a run that finished and was judged.
 * entry: { case, lens: null|string, run, error?, judge: {perFinding, otherMediumPlus}|null,
 *          verdict, cost, judgeCost }
 */
const usable = (e) => !e.error && e.judge?.perFinding;

/** Per-finding rows over a set of entries: one row per (case, finding) with each run's points. */
function findingRows(entries, cases) {
  const rows = [];
  for (const c of cases) {
    if (c.kind === "clean") continue;
    const mine = entries.filter((e) => e.case === c.id && usable(e));
    c.findings.forEach((f, i) => {
      const pts = mine.map((e) => POINTS[e.judge.perFinding[i]?.verdict ?? "missed"]);
      if (pts.length) rows.push({ case: c.id, kind: c.kind, index: i, class: f.class, severity: f.severity, pts });
    });
  }
  return rows;
}

/** Recall over rows: single-run (every finding-run) and any-run (best run per finding). */
export function recallOf(rows) {
  const all = rows.flatMap((r) => r.pts);
  return {
    findings: rows.length,
    runs: all.length,
    single: mean(all),
    any: mean(rows.map((r) => Math.max(...r.pts))),
    caughtAny: rows.filter((r) => r.pts.includes(1)).length,
  };
}

/**
 * The plain-run measures. Only entries with no lens count; failed or
 * unjudged runs are left out and listed.
 */
export function aggregate(entries, cases) {
  const plain = entries.filter((e) => !e.lens);
  const rows = findingRows(plain, cases);
  const byClass = {};
  for (const cls of Object.keys(CLASSES)) {
    const r = rows.filter((x) => x.class === cls);
    if (r.length) byClass[cls] = recallOf(r);
  }
  const byKind = {};
  for (const k of ["known", "missed"]) {
    const r = rows.filter((x) => x.kind === k);
    if (r.length) byKind[k] = recallOf(r);
  }
  const cleanIds = new Set(cases.filter((c) => c.kind === "clean").map((c) => c.id));
  const cleanRuns = plain.filter((e) => cleanIds.has(e.case) && usable(e));
  const perCleanCase = {};
  for (const id of cleanIds) {
    const r = cleanRuns.filter((e) => e.case === id);
    if (r.length) perCleanCase[id] = { runs: r.length, otherMediumPlus: sum(r.map((e) => e.judge.otherMediumPlus)), changesNeeded: r.filter((e) => e.verdict === "changes needed").length };
  }
  const otherRuns = plain.filter((e) => !cleanIds.has(e.case) && usable(e));
  return {
    recall: recallOf(rows),
    byClass,
    byKind,
    falseAlarms: {
      cleanRuns: cleanRuns.length,
      total: sum(cleanRuns.map((e) => e.judge.otherMediumPlus)),
      perRun: mean(cleanRuns.map((e) => e.judge.otherMediumPlus)),
      runsWithAny: cleanRuns.filter((e) => e.judge.otherMediumPlus > 0).length,
      runsChangesNeeded: cleanRuns.filter((e) => e.verdict === "changes needed").length,
      perCase: perCleanCase,
    },
    otherOnFindingCases: { runs: otherRuns.length, perRun: mean(otherRuns.map((e) => e.judge.otherMediumPlus)) },
    cost: {
      reviews: sum(entries.map((e) => e.cost ?? 0)),
      judge: sum(entries.map((e) => e.judgeCost ?? 0)),
      total: sum(entries.map((e) => (e.cost ?? 0) + (e.judgeCost ?? 0))),
    },
    failures: entries.filter((e) => !usable(e)).map((e) => ({ case: e.case, lens: e.lens ?? null, run: e.run, error: e.error ?? "not judged" })),
  };
}

/** Rule used for the lens verdict: the lens pair must add at least this many points over one run. */
export const LENS_GAIN_POINTS = 0.15;

/**
 * One plain run vs two plain runs vs a security-lens run plus a
 * concurrency-lens run, on the findings of classes A and B in cases where
 * every one of those runs was judged. Pairs use the first two plain runs.
 */
export function lensComparison(entries, cases) {
  const ab = cases.filter((c) => c.kind !== "clean" && c.findings.some((f) => f.class === "A" || f.class === "B"));
  const sec = (id) => entries.find((e) => e.case === id && e.lens === "security" && usable(e));
  const con = (id) => entries.find((e) => e.case === id && e.lens === "concurrency" && usable(e));
  const plainOf = (id) => entries.filter((e) => e.case === id && !e.lens && usable(e)).sort((a, b) => a.run - b.run).slice(0, 2);
  const rows = [];
  let n = 0;
  const cost = { plainPair: 0, lensPair: 0, plainOne: 0 };
  for (const c of ab) {
    const p = plainOf(c.id);
    const s = sec(c.id);
    const k = con(c.id);
    if (p.length < 2 || !s || !k) continue;
    n++;
    cost.plainPair += (p[0].cost ?? 0) + (p[1].cost ?? 0);
    cost.plainOne += ((p[0].cost ?? 0) + (p[1].cost ?? 0)) / 2;
    cost.lensPair += (s.cost ?? 0) + (k.cost ?? 0);
    c.findings.forEach((f, i) => {
      if (f.class !== "A" && f.class !== "B") return;
      const pt = (e) => POINTS[e.judge.perFinding[i]?.verdict ?? "missed"];
      rows.push({
        case: c.id, class: f.class,
        plain: p.map(pt), lens: [pt(s), pt(k)],
      });
    });
  }
  if (!rows.length) return { cases: 0, findings: 0, note: "no case has all four runs judged" };
  const one = mean(rows.flatMap((r) => r.plain));
  const plainPair = mean(rows.map((r) => Math.max(...r.plain)));
  const lensPair = mean(rows.map((r) => Math.max(...r.lens)));
  const lensEach = mean(rows.flatMap((r) => r.lens));
  const gainOverOne = lensPair - one;
  const gainOverPair = lensPair - plainPair;
  let conclusion;
  if (gainOverOne >= LENS_GAIN_POINTS && gainOverPair > 0) conclusion = "justified";
  else if (gainOverOne >= LENS_GAIN_POINTS) conclusion = "two runs help, the lenses do not add to a plain second run";
  else conclusion = "not justified";
  return { cases: n, findings: rows.length, onePlainRun: one, plainPair, lensPair, lensEach, gainOverOne, gainOverPair, cost, conclusion, rows };
}

const pct = (x) => (x == null ? "n/a" : `${(x * 100).toFixed(0)}%`);
const usd = (x) => `$${(x ?? 0).toFixed(2)}`;

/** The conclusion sentence for the lens section, with its numbers. */
export function lensSentence(l) {
  if (!l.findings) return `Not measured: ${l.note}.`;
  const nums = `On ${l.findings} class A/B findings in ${l.cases} cases, one plain run catches ${pct(l.onePlainRun)}, two plain runs ${pct(l.plainPair)}, a security-lens run plus a concurrency-lens run ${pct(l.lensPair)} (cost of the pair ${usd(l.cost.lensPair)} against ${usd(l.cost.plainOne)} for one plain run).`;
  const rule = `Rule: justified when the lens pair beats one run by at least ${LENS_GAIN_POINTS * 100} points and beats two plain runs.`;
  const verdict = { justified: "Conclusion: the extra run is justified on risky diffs.", "not justified": "Conclusion: not justified by this sample." }[l.conclusion] ?? `Conclusion: ${l.conclusion}.`;
  return `${nums} ${verdict} ${rule}`;
}

/** The markdown report. */
export function renderReport(summary, entries, cases, meta) {
  const L = [];
  const a = summary.agg;
  L.push(`# Review replay eval: ${meta.date}`, "");
  L.push(`- Prompt: \`${meta.prompt}\` (sha256 ${meta.promptSha.slice(0, 12)})`);
  L.push(`- Review model: ${meta.model}; judge: ${meta.judgeModel}; runs per case: ${meta.runs}; cases: ${cases.length}`);
  L.push(`- Total cost: ${usd(a.cost.total)} (reviews ${usd(a.cost.reviews)}, judge ${usd(a.cost.judge)})`, "");
  L.push("## Recall", "");
  L.push(`Counted over the ${a.recall.findings} known findings (caught = 1, partly = 0.5). Single-run recall averages every run; any-run recall takes the best run per finding, so the pair is what a second run adds.`, "");
  L.push("| Group | Findings | Single-run recall | Any-run recall |", "| --- | ---: | ---: | ---: |");
  L.push(`| **All** | ${a.recall.findings} | ${pct(a.recall.single)} | ${pct(a.recall.any)} |`);
  for (const [k, r] of Object.entries(a.byKind)) L.push(`| kind ${k} | ${r.findings} | ${pct(r.single)} | ${pct(r.any)} |`);
  for (const [k, r] of Object.entries(a.byClass)) L.push(`| class ${k}, ${CLASSES[k]} | ${r.findings} | ${pct(r.single)} | ${pct(r.any)} |`);
  L.push("", "## False alarms", "");
  const f = a.falseAlarms;
  L.push(`On clean cases (${f.cleanRuns} judged runs): **${f.perRun == null ? "n/a" : f.perRun.toFixed(2)} other Medium+ findings per run** (${f.total} in all); ${f.runsWithAny} runs had at least one; ${f.runsChangesNeeded} ended \`Review: changes needed\`.`, "");
  L.push("| Clean case | Runs | Medium+ findings | Ended changes needed |", "| --- | ---: | ---: | ---: |");
  for (const [id, r] of Object.entries(f.perCase)) L.push(`| ${id} | ${r.runs} | ${r.otherMediumPlus} | ${r.changesNeeded} |`);
  if (a.otherOnFindingCases.runs) L.push("", `On the other cases the review also lists ${a.otherOnFindingCases.perRun.toFixed(2)} further Medium+ findings per run (not scored; they may be real).`);
  L.push("", "## Per case", "");
  L.push("| Case | Kind | Finding (class) | Verdicts by run | Other Medium+ by run | Review verdicts | Denied tool calls by run |", "| --- | --- | --- | --- | --- | --- | --- |");
  for (const c of cases) {
    const mine = entries.filter((e) => e.case === c.id && !e.lens).sort((x, y) => x.run - y.run);
    const others = mine.map((e) => (usable(e) ? e.judge.otherMediumPlus : "x")).join(", ");
    const verdicts = mine.map((e) => e.verdict ?? "x").join(", ");
    const denied = mine.map((e) => e.denials ?? "n/a").join(", ");
    if (c.kind === "clean") L.push(`| ${c.id} | clean | none | | ${others} | ${verdicts} | ${denied} |`);
    else c.findings.forEach((fd, i) => {
      const v = mine.map((e) => (usable(e) ? e.judge.perFinding[i].verdict : "failed")).join(", ");
      L.push(`| ${i === 0 ? c.id : ""} | ${i === 0 ? c.kind : ""} | ${fd.severity} (${fd.class}) | ${v} | ${i === 0 ? others : ""} | ${i === 0 ? verdicts : ""} | ${i === 0 ? denied : ""} |`);
    });
  }
  L.push("", "## Two lenses vs one run", "");
  L.push(lensSentence(summary.lens), "");
  if (summary.lens.findings) {
    L.push("| Mode | Recall on the A/B findings |", "| --- | ---: |");
    L.push(`| One plain run (mean of runs 1 and 2) | ${pct(summary.lens.onePlainRun)} |`);
    L.push(`| Two plain runs (best of runs 1 and 2) | ${pct(summary.lens.plainPair)} |`);
    L.push(`| Each lens run alone (mean) | ${pct(summary.lens.lensEach)} |`);
    L.push(`| Security run + concurrency run (best of the two) | ${pct(summary.lens.lensPair)} |`);
  }
  L.push("", "## Failures", "");
  if (a.failures.length) for (const x of a.failures) L.push(`- ${x.case}${x.lens ? ` (${x.lens})` : ""} run ${x.run}: ${x.error}`);
  else L.push("None.");
  L.push("");
  return L.join("\n");
}
