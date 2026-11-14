// Runs `claude -p` with the prompt on stdin and reads its JSON result.

import { spawn } from "node:child_process";

/**
 * Resolves `{ code, text, cost, usage, durationMs, isError, raw }`. Never
 * rejects for a failing session: the caller reads `code`/`isError`.
 * CLAUDE_BIN replaces the command.
 */
export function runClaude({ args, input, cwd, timeoutMs }) {
  return new Promise((resolve) => {
    const bin = process.env.CLAUDE_BIN ?? "claude";
    const child = spawn(bin, ["-p", "--output-format", "json", "--no-session-persistence", ...args], { cwd, stdio: ["pipe", "pipe", "pipe"] });
    let out = "";
    let err = "";
    let timedOut = false;
    const timer = setTimeout(() => {
      timedOut = true;
      child.kill("SIGKILL");
    }, timeoutMs);
    child.stdout.on("data", (d) => (out += d));
    child.stderr.on("data", (d) => (err += d));
    child.on("error", (e) => {
      clearTimeout(timer);
      resolve({ code: -1, text: "", cost: 0, usage: null, durationMs: 0, isError: true, raw: String(e), stderr: String(e) });
    });
    child.on("close", (code) => {
      clearTimeout(timer);
      let j = null;
      try {
        j = JSON.parse(out);
      } catch {}
      resolve({
        code: timedOut ? -2 : code,
        timedOut,
        text: typeof j?.result === "string" ? j.result : "",
        cost: j?.total_cost_usd ?? 0,
        usage: j?.usage ?? null,
        durationMs: j?.duration_ms ?? 0,
        isError: Boolean(j?.is_error) || !j,
        raw: j ? undefined : out.slice(0, 2000),
        stderr: err.slice(-2000),
      });
    });
    child.stdin.end(input);
  });
}
