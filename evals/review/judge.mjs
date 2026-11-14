// The Sonnet judge: scores one review against a case's known findings.

import fs from "node:fs";
import os from "node:os";
import { runClaude } from "./claude.mjs";
import { buildJudgePrompt, parseJudge } from "./score.mjs";

/** Resolves `{ ok, perFinding, otherMediumPlus, cost, raw? }` or `{ ok: false, error, cost }`. */
export async function judgeReview({ findings, reviewText, model = "sonnet", timeoutMs = 10 * 60 * 1000 }) {
  const cwd = fs.mkdtempSync(`${os.tmpdir()}/review-judge-`);
  let cost = 0;
  try {
    for (let attempt = 0; attempt < 2; attempt++) {
      const r = await runClaude({ args: ["--model", model, "--tools", ""], input: buildJudgePrompt(findings, reviewText), cwd, timeoutMs });
      cost += r.cost;
      if (r.code !== 0 || r.isError) {
        if (attempt === 1) return { ok: false, error: `judge exited ${r.code}: ${r.stderr || r.raw || ""}`.trim(), cost };
        continue;
      }
      const p = parseJudge(r.text, findings.length);
      if (p.ok || attempt === 1) return { ...p, cost, raw: p.ok ? undefined : r.text.slice(0, 2000) };
    }
  } finally {
    fs.rmSync(cwd, { recursive: true, force: true });
  }
}
