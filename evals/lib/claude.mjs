// What the evals share: how to run `claude -p` with this repo's
// ganymede-journal plugin. The journal evals (evals/journal) and the
// person-and-agent evals (evals/agent-native) both build their command
// here, so CLAUDE_BIN, CLAUDE_ARGS and GANYMEDE_BIN mean the same in each.

import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));

/** The ganymede-journal plugin directory in this checkout. */
export const PLUGIN_DIR = path.resolve(here, "../../plugins/ganymede-journal");

/** Tools a session may use without asking: the ganymede tools, file tools, read-only shell. */
export const ALLOWED_TOOLS = [
  "mcp__plugin_ganymede-journal_ganymede",
  "Read",
  "Edit",
  "Write",
  "Bash(grep:*)",
  "Bash(wc:*)",
  "Bash(head:*)",
  "Bash(ls:*)",
];

/**
 * The command for one headless session: `{bin, args, env}`. `extraAllowed`
 * adds to the default `--allowedTools`; CLAUDE_ARGS (space separated)
 * replaces the extra arguments, so it replaces the allow list too.
 * `GANYMEDE_BIN`'s directory goes first on PATH, so the plugin's
 * `ganymede mcp` and its hooks find that binary.
 */
export function claudeInvocation(prompt, { env = process.env, extraAllowed = [] } = {}) {
  const bin = env.CLAUDE_BIN ?? "claude";
  const extra = env.CLAUDE_ARGS?.split(" ").filter(Boolean) ?? [
    "--allowedTools",
    [...ALLOWED_TOOLS, ...extraAllowed].join(","),
  ];
  const out = { ...env };
  if (env.GANYMEDE_BIN) {
    out.PATH = `${path.dirname(path.resolve(env.GANYMEDE_BIN))}${path.delimiter}${out.PATH}`;
  }
  return { bin, args: ["-p", prompt, "--plugin-dir", PLUGIN_DIR, ...extra], env: out };
}

/** A session's timeout in milliseconds: EVAL_TIMEOUT_MS, else `fallback`. */
export function evalTimeoutMs(fallback) {
  return Number(process.env.EVAL_TIMEOUT_MS ?? fallback);
}
