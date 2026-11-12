#!/usr/bin/env node
// ganymede-journal's hooks. One script, run as `journal.mjs <event>` with
// the hook's JSON on stdin:
//
//   session-start   report the Ganymede server and any open journal
//   prompt          (UserPromptSubmit) cells others changed since the
//                   agent's last read, as context
//   post-tool       (PostToolUse) follow the agent's notebook tools;
//                   record a Bash command that ends in `# journal`
//   stop            block stopping, once per round of edits, while the
//                   journal has edits since its last clean run-all
//
// It reaches the server with `ganymede mcp call`, which finds the server
// the agent's `ganymede mcp` uses. State per Claude Code session (the open
// notebook, its read mark, whether it changed since a clean run-all) is a
// JSON file in the plugin's data dir. Any failure is silent: a hook must
// never get in the way of the session.
//
// Settings, as environment variables:
//   GANYMEDE_BIN               the ganymede binary (default `ganymede`)
//   GANYMEDE_JOURNAL_RECORD    `marked` (default): record Bash commands
//                              that end in `# journal`; `all`; `off`
//   GANYMEDE_JOURNAL_STOP      `off` turns the stop check off

import { execFileSync } from "node:child_process";
import { mkdirSync, readFileSync, realpathSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

/** Bash output kept in a recorded cell. */
export const OUTPUT_LIMIT = 2000;

const TOOL = /^mcp__(?:plugin_ganymede-journal_ganymede|ganymede)__(\w+)$/;
const EDITS = new Set(["cell_add", "cell_edit", "cell_delete", "cell_move"]);

export function stateFile(env, sessionId) {
  const dir = join(env.CLAUDE_PLUGIN_DATA || join(tmpdir(), "ganymede-journal"), "sessions");
  const safe = String(sessionId || "default").replace(/[^\w.-]/g, "_");
  return { dir, file: join(dir, `${safe}.json`) };
}

function load(env, sessionId) {
  try {
    return JSON.parse(readFileSync(stateFile(env, sessionId).file, "utf8"));
  } catch {
    return {};
  }
}

function save(env, sessionId, state) {
  const { dir, file } = stateFile(env, sessionId);
  mkdirSync(dir, { recursive: true });
  writeFileSync(file, JSON.stringify(state), { mode: 0o600 });
}

/** `ganymede mcp call`: its stdout, or null when it failed. */
function call(env, cwd, tool, args, timeout = 20_000) {
  try {
    return execFileSync(
      env.GANYMEDE_BIN || "ganymede",
      ["mcp", "call", tool, JSON.stringify(args), "--client", "claude-code"],
      { cwd, env, timeout, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] },
    );
  } catch {
    return null;
  }
}

/** The text of a tool's result, whatever shape the hook got it in. */
export function responseText(r) {
  if (r == null) return "";
  if (typeof r === "string") return r;
  if (Array.isArray(r)) return r.map(responseText).join("\n");
  if (Array.isArray(r.content)) return responseText(r.content);
  if (typeof r.text === "string") return r.text;
  if (typeof r.stdout === "string" || typeof r.stderr === "string") {
    return [r.stdout, r.stderr].filter(Boolean).join("\n");
  }
  return JSON.stringify(r);
}

export function readMark(text) {
  const m = /^@@read (\S+)$/m.exec(text);
  return m ? m[1] : null;
}

/** Whether a run-all reply shows every cell ran without error. */
export function cleanRun(text) {
  return !/^Timed out/m.test(text) && !/"output_type":\s*"error"/.test(text);
}

export function shouldRecord(command, mode) {
  if (mode === "off" || !command) return false;
  // The hook's own calls, and other ganymede commands, aren't steps.
  if (/^\s*(\S*\/)?ganymede\s/.test(command)) return false;
  if (mode === "all") return true;
  return /#\s*journal\s*$/.test(command.trimEnd());
}

export function recordedSource(command, output) {
  const cmd = command.replace(/\s*#\s*journal\s*$/, "");
  let out = output.trimEnd();
  if (out.length > OUTPUT_LIMIT) {
    out = `${out.slice(0, OUTPUT_LIMIT)}\n… (${out.length - OUTPUT_LIMIT} more characters)`;
  }
  const fence = "```";
  let s = `Recorded: ran in the shell, not in this kernel.\n\n${fence}sh\n$ ${cmd}\n${fence}\n`;
  if (out) s += `\n${fence}text\n${out}\n${fence}\n`;
  return s;
}

function context(event, text) {
  return { hookSpecificOutput: { hookEventName: event, additionalContext: text } };
}

/** The hook for `event`: returns what to print (a string or an object). */
export function handle(event, input, env = process.env) {
  const sid = input.session_id;
  const cwd = input.cwd || process.cwd();
  const state = load(env, sid);
  // The folder the session started in, where the agent's `ganymede mcp`
  // runs: tool paths are relative to it (#730), and `ganymede mcp call`
  // resolves them against its own folder, which a `cd` in Bash would
  // move (#732). Kept from session start, or from the journal's open.
  const home = () => state.home || cwd;
  switch (event) {
    case "session-start": {
      if (!state.home) {
        state.home = cwd;
        save(env, sid, state);
      }
      const lines = [];
      const kernels = call(env, home(), "kernelspecs_list", {}, 10_000);
      if (kernels === null) {
        lines.push(
          "Ganymede: no server is running for this directory, or `ganymede` isn't installed. " +
            "The ganymede tools start a server when first used. For a person to watch the " +
            "notebook live, run `ganymede serve --mcp` here.",
        );
      } else {
        lines.push(`Ganymede: a server is running for this directory. Kernels:\n${kernels.trim()}`);
      }
      if (state.path) {
        lines.push(
          `Journal open in this session: ${state.path}` +
            (state.mark ? ` (last read mark ${state.mark})` : "") +
            ". Read it again before building on it.",
        );
      }
      return lines.join("\n");
    }
    case "post-tool": {
      const name = input.tool_name || "";
      const text = responseText(input.tool_response);
      const args = input.tool_input || {};
      const tool = TOOL.exec(name)?.[1];
      if (tool) {
        const path = args.path || state.path;
        if (tool === "notebook_open" || tool === "notebook_read") {
          const mark = readMark(text);
          if (mark && path) {
            if (path !== state.path) state.dirty = false;
            // `home` is set at session-start; without one (the plugin
            // enabled mid-session, or older state) the folder at this
            // open is a best effort.
            Object.assign(state, { path, mark, home: home() });
          }
        } else if (path && path === state.path) {
          if (EDITS.has(tool) || (tool === "cell_run" && args.record !== false)) {
            Object.assign(state, { dirty: true, nagged: false });
          } else if (tool === "notebook_run_all") {
            state.dirty = !cleanRun(text);
            state.nagged = false;
          }
        }
        save(env, sid, state);
        return "";
      }
      if (name === "Bash" && state.path) {
        const command = args.command || "";
        if (!shouldRecord(command, env.GANYMEDE_JOURNAL_RECORD || "marked")) return "";
        const source = recordedSource(command, text);
        const out = call(env, home(), "cell_add", {
          path: state.path,
          source,
          mode: "recorded",
          ...(state.mark ? { mark: state.mark } : {}),
        });
        if (out === null) return "";
        Object.assign(state, { dirty: true, nagged: false });
        save(env, sid, state);
        return context(
          "PostToolUse",
          `ganymede-journal recorded this command in ${state.path} (${out.trim()}); don't add it again.`,
        );
      }
      return "";
    }
    case "prompt": {
      if (!state.path || !state.mark) return "";
      const out = call(env, home(), "notebook_read", { path: state.path, since: state.mark });
      if (out === null || out.startsWith("No changes")) return "";
      if (out.startsWith("Read mark")) {
        state.mark = readMark(out) || state.mark;
        save(env, sid, state);
        return context(
          "UserPromptSubmit",
          `The Ganymede server lost track of your last read of ${state.path} (it restarted). ` +
            "Read the notebook again before building on it.",
        );
      }
      Object.assign(state, { dirty: true, nagged: false });
      save(env, sid, state);
      return context(
        "UserPromptSubmit",
        `The journal ${state.path} changed since your last read, by a person or another ` +
          `agent. Work from what is there now:\n\n${out.trim()}`,
      );
    }
    case "stop": {
      if (env.GANYMEDE_JOURNAL_STOP === "off" || input.stop_hook_active) return "";
      if (!state.path || !state.dirty || state.nagged) return "";
      state.nagged = true;
      save(env, sid, state);
      return {
        decision: "block",
        reason:
          `The journal ${state.path} has changes since its last clean run-all. If the analysis ` +
          "is finished, run notebook_run_all, fix what fails (or say why it can't reproduce), " +
          "and update the summary. If you are only pausing for the user, say so and stop.",
      };
    }
    default:
      return "";
  }
}

async function main() {
  const event = process.argv[2];
  let raw = "";
  for await (const chunk of process.stdin) raw += chunk;
  let out = "";
  try {
    out = handle(event, raw ? JSON.parse(raw) : {});
  } catch {
    out = "";
  }
  if (typeof out === "string") {
    if (out) process.stdout.write(`${out}\n`);
  } else {
    process.stdout.write(JSON.stringify(out));
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(realpathSync(process.argv[1])).href) {
  await main();
}
