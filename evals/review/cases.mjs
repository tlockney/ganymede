// Case files and the prompt a review run gets. Pure: no model calls, no git.

import fs from "node:fs";
import path from "node:path";

/** The defect classes of the 2026-11-14 review-patterns analysis. */
export const CLASSES = {
  A: "Concurrency, ordering, lifecycle",
  B: "Security boundaries",
  C: "Not every entry point or consumer covered",
  D: "Docs and process",
  E: "Tests that don't prove the claim",
  F: "Errors swallowed or replaced",
  G: "Platform semantics",
  H: "Version skew and release wiring",
  I: "Edge-case input",
};

/**
 * known: a posted review's first round found the finding.
 * missed: a passing review lacked it and a later run found it.
 * clean: passed with nothing to fix; every Medium+ finding is a false alarm.
 */
export const KINDS = ["known", "missed", "clean"];
export const SEVERITIES = ["Medium", "High"];

/** Lenses for `--lens`: the one extra line a lens run adds. */
export const LENSES = {
  security: "security boundaries",
  concurrency: "concurrency and lifecycle",
};

const SHA = /^[0-9a-f]{40}$/;

/** Problems with one case object, as strings; empty when it is valid. */
export function validateCase(c, file = "") {
  const p = [];
  const need = (ok, msg) => ok || p.push(msg);
  need(c && typeof c === "object" && !Array.isArray(c), "not an object");
  if (p.length) return p;
  need(typeof c.id === "string" && /^[a-z0-9][a-z0-9-]*$/.test(c.id), "id must be lowercase letters, digits and dashes");
  if (file) need(c.id === path.basename(file, ".json"), `id ${c.id} must match the file name`);
  need(typeof c.repo === "string" && /^tlockney\/[\w.-]+$/.test(c.repo), "repo must be tlockney/<name>");
  need(Number.isInteger(c.pr) && c.pr > 0, "pr must be a positive integer");
  need(Number.isInteger(c.issue) && c.issue > 0, "issue must be a positive integer");
  need(typeof c.base === "string" && SHA.test(c.base), "base must be a full 40-hex sha");
  need(typeof c.head === "string" && SHA.test(c.head), "head must be a full 40-hex sha");
  need(c.base !== c.head, "base and head must differ");
  need(c.branch === undefined || (typeof c.branch === "string" && c.branch.length > 0), "branch, when given, must be a non-empty string");
  need(KINDS.includes(c.kind), `kind must be one of ${KINDS.join(", ")}`);
  need(typeof c.notes === "string", "notes must be a string (may be empty)");
  need(Array.isArray(c.findings), "findings must be an array");
  if (Array.isArray(c.findings)) {
    if (c.kind === "clean") need(c.findings.length === 0, "a clean case has no findings");
    else need(c.findings.length > 0, `a ${c.kind} case needs at least one finding`);
    c.findings.forEach((f, i) => {
      const at = `findings[${i}]`;
      need(f && typeof f.summary === "string" && f.summary.length >= 40, `${at}.summary must describe the defect (at least 40 characters)`);
      need(f && SEVERITIES.includes(f.severity), `${at}.severity must be one of ${SEVERITIES.join(", ")}`);
      need(f && f.class in CLASSES, `${at}.class must be one of ${Object.keys(CLASSES).join("")}`);
      need(f && typeof f.source === "string" && f.source.length > 0, `${at}.source must say where the finding came from`);
    });
  }
  return p;
}

/** Loads and validates every `*.json` in dir; throws listing all problems. */
export function loadCases(dir) {
  const files = fs.readdirSync(dir).filter((f) => f.endsWith(".json")).sort();
  const cases = [];
  const problems = [];
  for (const f of files) {
    let c;
    try {
      c = JSON.parse(fs.readFileSync(path.join(dir, f), "utf8"));
    } catch (e) {
      problems.push(`${f}: ${e.message}`);
      continue;
    }
    for (const m of validateCase(c, f)) problems.push(`${f}: ${m}`);
    cases.push(c);
  }
  const seen = new Set();
  for (const c of cases) {
    if (seen.has(c.id)) problems.push(`duplicate id ${c.id}`);
    seen.add(c.id);
  }
  if (problems.length) throw new Error(`invalid cases:\n${problems.join("\n")}`);
  return cases;
}

/** Cases whose id matches any comma-separated pattern (`*` is a wildcard); all when none. */
export function selectCases(cases, spec) {
  if (!spec) return cases;
  const res = spec.split(",").map((s) => s.trim()).filter(Boolean).map(
    (s) => new RegExp(`^${s.replace(/[.+?^${}()|[\]\\]/g, "\\$&").replace(/\*/g, ".*")}$`),
  );
  const picked = cases.filter((c) => res.some((r) => r.test(c.id)));
  if (!picked.length) throw new Error(`no case matches ${spec}`);
  return picked;
}

/** The branch name the trailer gives: the PR's head branch, else `eval/<id>`. */
export const branchOf = (c) => c.branch ?? `eval/${c.id}`;

/** The extra line a lens run adds after the prompt text. */
export function lensLine(lens) {
  if (!(lens in LENSES)) throw new Error(`unknown lens ${lens} (use ${Object.keys(LENSES).join(" or ")})`);
  return `Lens for this run: look hardest at ${LENSES[lens]}.`;
}

/**
 * What a review run is given: the prompt file word for word, an optional
 * lens line, then the trailer the workflow adds (REPO and clone path,
 * BRANCH, ISSUE, DIFF) and nothing else.
 */
export function assemblePrompt(promptText, { lens = null, repo, clone, branch, issue, diff }) {
  const head = promptText.replace(/\n*$/, "\n");
  const extra = lens ? `\n${lensLine(lens)}\n` : "";
  const trailer = `\nREPO: ${repo} (local clone: ${clone})\nBRANCH: ${branch}\nISSUE: ${issue}\nDIFF (git diff origin/main...HEAD):\n${diff}`;
  return head + extra + trailer;
}
