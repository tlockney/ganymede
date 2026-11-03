// Tests for journal.mjs: `node --test plugins/ganymede-journal/hooks/`.
//
// The unit tests run the hooks against a fake `ganymede` that logs its
// calls. The end-to-end test runs them against a real `ganymede mcp`
// when GANYMEDE_E2E_BIN names a built binary, and skips otherwise.

import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { chmodSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createInterface } from "node:readline";
import { test } from "node:test";
import {
  cleanRun,
  handle,
  OUTPUT_LIMIT,
  readMark,
  recordedSource,
  responseText,
  shouldRecord,
} from "./journal.mjs";

const TOOL = "mcp__plugin_ganymede-journal_ganymede__";

/** A fake ganymede: logs `mcp call` arguments, answers from a table. */
function fake(answers) {
  const dir = mkdtempSync(join(tmpdir(), "journal-hook-"));
  const log = join(dir, "calls.log");
  const bin = join(dir, "ganymede");
  writeFileSync(
    bin,
    `#!/usr/bin/env node
const fs = require("node:fs");
const [, , , , tool, args] = process.argv;
fs.appendFileSync(${JSON.stringify(log)}, JSON.stringify({ tool, args: JSON.parse(args) }) + "\\n");
const answers = ${JSON.stringify(answers)};
if (!(tool in answers)) { process.stderr.write("no server"); process.exit(1); }
process.stdout.write(answers[tool]);
`,
  );
  chmodSync(bin, 0o755);
  const env = { ...process.env, GANYMEDE_BIN: bin, CLAUDE_PLUGIN_DATA: join(dir, "data") };
  delete env.GANYMEDE_JOURNAL_RECORD;
  delete env.GANYMEDE_JOURNAL_STOP;
  const calls = () => {
    try {
      return readFileSync(log, "utf8").trim().split("\n").filter(Boolean).map((l) => JSON.parse(l));
    } catch {
      return [];
    }
  };
  return { env, calls, dir };
}

const base = (dir) => ({ session_id: "s1", cwd: dir });

test("helpers", () => {
  assert.equal(readMark("@@cell {}\nx\n@@read abc123\n"), "abc123");
  assert.equal(readMark("no mark"), null);
  assert.ok(cleanRun('@@cell {}\n@@output {"output_type":"stream"}\n'));
  assert.ok(!cleanRun('@@output {"output_type":"error"}\n'));
  assert.ok(!cleanRun("Timed out; cells are still running.\n"));
  assert.equal(responseText([{ type: "text", text: "a" }, { type: "text", text: "b" }]), "a\nb");
  assert.equal(responseText({ content: [{ type: "text", text: "c" }] }), "c");
  assert.equal(responseText({ stdout: "out", stderr: "err" }), "out\nerr");
  assert.equal(responseText("plain"), "plain");
  assert.ok(shouldRecord("gh run list > runs.json # journal", "marked"));
  assert.ok(!shouldRecord("ls", "marked"));
  assert.ok(shouldRecord("ls", "all"));
  assert.ok(!shouldRecord("ls # journal", "off"));
  assert.ok(!shouldRecord("ganymede mcp call x # journal", "all"));
  const src = recordedSource("echo hi # journal", "x".repeat(OUTPUT_LIMIT + 5));
  assert.ok(src.includes("$ echo hi\n"), src);
  assert.ok(!src.includes("# journal"), src);
  assert.ok(src.includes("… (5 more characters)"), src);
});

test("session-start reports the server, or its absence, and the open journal", () => {
  const up = fake({ kernelspecs_list: "default: cdp-node\ncdp-node: Node (typescript)\n" });
  let out = handle("session-start", base(up.dir), up.env);
  assert.match(out, /a server is running/);
  assert.match(out, /cdp-node: Node/);
  const down = fake({});
  out = handle("session-start", base(down.dir), down.env);
  assert.match(out, /no server is running/);
  // A resumed session names its journal.
  handle(
    "post-tool",
    { ...base(down.dir), tool_name: `${TOOL}notebook_open`, tool_input: { path: "j.ipynb" },
      tool_response: [{ type: "text", text: "Created j.ipynb\n\n@@read m1\n" }] },
    down.env,
  );
  out = handle("session-start", base(down.dir), down.env);
  assert.match(out, /Journal open in this session: j\.ipynb \(last read mark m1\)/);
});

test("prompt passes on others' changes, and nothing else", () => {
  const f = fake({
    notebook_read:
      'Changed by others since that read: 1 added, 0 edited, 0 moved, 0 deleted.\n\n@@cell {"change":["added"],"id":"p"}\nP\n\n@@read m1\n',
  });
  // No journal open: no call.
  assert.equal(handle("prompt", base(f.dir), f.env), "");
  assert.equal(f.calls().length, 0);
  handle(
    "post-tool",
    { ...base(f.dir), tool_name: `${TOOL}notebook_read`, tool_input: { path: "j.ipynb" },
      tool_response: [{ type: "text", text: "@@cell {}\n@@read m1\n" }] },
    f.env,
  );
  const out = handle("prompt", base(f.dir), f.env);
  assert.equal(out.hookSpecificOutput.hookEventName, "UserPromptSubmit");
  assert.match(out.hookSpecificOutput.additionalContext, /j\.ipynb changed since your last read/);
  assert.match(out.hookSpecificOutput.additionalContext, /"change":\["added"\]/);
  assert.deepEqual(f.calls()[0], { tool: "notebook_read", args: { path: "j.ipynb", since: "m1" } });

  const quiet = fake({ notebook_read: "No changes by others since that read.\n@@read m1\n" });
  handle(
    "post-tool",
    { ...base(quiet.dir), tool_name: `${TOOL}notebook_open`, tool_input: { path: "j.ipynb" },
      tool_response: "Opened j.ipynb\n@@read m1\n" },
    quiet.env,
  );
  assert.equal(handle("prompt", base(quiet.dir), quiet.env), "");

  const lost = fake({ notebook_read: "Read mark m1 is unknown here …\n@@read m2\n" });
  handle(
    "post-tool",
    { ...base(lost.dir), tool_name: `${TOOL}notebook_open`, tool_input: { path: "j.ipynb" },
      tool_response: "@@read m1\n" },
    lost.env,
  );
  const o = handle("prompt", base(lost.dir), lost.env);
  assert.match(o.hookSpecificOutput.additionalContext, /lost track/);
  // The new mark is used next time.
  handle("prompt", base(lost.dir), lost.env);
  assert.equal(lost.calls()[1].args.since, "m2");
});

test("post-tool records marked Bash commands into the open journal", () => {
  const f = fake({ cell_add: "Added cell abc" });
  const bash = (command) =>
    handle(
      "post-tool",
      { ...base(f.dir), tool_name: "Bash", tool_input: { command },
        tool_response: { stdout: "450 runs", stderr: "" } },
      f.env,
    );
  // No journal open: nothing recorded.
  assert.equal(bash("gh run list # journal"), "");
  handle(
    "post-tool",
    { ...base(f.dir), tool_name: `${TOOL}notebook_open`, tool_input: { path: "j.ipynb" },
      tool_response: "@@read m1\n" },
    f.env,
  );
  assert.equal(bash("ls"), "", "unmarked");
  const out = bash("gh run list > runs.json # journal");
  assert.match(out.hookSpecificOutput.additionalContext, /recorded this command in j\.ipynb \(Added cell abc\)/);
  const [c] = f.calls();
  assert.equal(c.tool, "cell_add");
  assert.equal(c.args.mode, "recorded");
  assert.equal(c.args.mark, "m1");
  assert.match(c.args.source, /\$ gh run list > runs\.json\n/);
  assert.match(c.args.source, /450 runs/);
  // `all` records unmarked commands too.
  handle(
    "post-tool",
    { ...base(f.dir), tool_name: "Bash", tool_input: { command: "ls" }, tool_response: "" },
    { ...f.env, GANYMEDE_JOURNAL_RECORD: "all" },
  );
  assert.equal(f.calls().length, 2);
});

test("stop blocks once per round of edits since a clean run-all", () => {
  const f = fake({});
  const post = (tool, input, text) =>
    handle(
      "post-tool",
      { ...base(f.dir), tool_name: `${TOOL}${tool}`, tool_input: input,
        tool_response: [{ type: "text", text }] },
      f.env,
    );
  const stop = (active = false) =>
    handle("stop", { ...base(f.dir), stop_hook_active: active }, f.env);
  assert.equal(stop(), "", "no journal");
  post("notebook_open", { path: "j.ipynb" }, "@@read m1\n");
  assert.equal(stop(), "", "nothing edited yet");
  post("cell_run", { source: "1", record: false }, "1");
  assert.equal(stop(), "", "a scratch run is no edit");
  post("cell_add", { source: "note" }, "Added cell a");
  assert.equal(stop(true), "", "already continuing from a stop hook");
  const blocked = stop();
  assert.equal(blocked.decision, "block");
  assert.match(blocked.reason, /notebook_run_all/);
  assert.equal(stop(), "", "only once");
  post("cell_edit", { id: "a", source: "more" }, "Edited cell a");
  assert.equal(stop().decision, "block", "edits after a block are a new round");
  assert.equal(stop(), "", "once for that round too");
  post("notebook_run_all", {}, '@@output {"output_type":"error"}\n');
  assert.equal(stop().decision, "block", "a failing run-all isn't clean");
  post("notebook_run_all", {}, "@@cell {}\n");
  assert.equal(stop(), "", "clean");
  post("cell_edit", { id: "a", source: "x" }, "Edited cell a");
  assert.equal(handle("stop", base(f.dir), { ...f.env, GANYMEDE_JOURNAL_STOP: "off" }), "");
});

// End to end, on a real server: the agent's `ganymede mcp` (own server),
// a Bash step recorded by the hook, a person's edit through another
// connection reported on the next prompt, and the hook's own cell not.
test("hooks against a real ganymede mcp", { skip: !process.env.GANYMEDE_E2E_BIN }, async () => {
  const bin = process.env.GANYMEDE_E2E_BIN;
  const root = mkdtempSync(join(tmpdir(), "journal-e2e-"));
  const state = mkdtempSync(join(tmpdir(), "journal-e2e-state-"));
  const env = {
    ...process.env,
    GANYMEDE_BIN: bin,
    CLAUDE_PLUGIN_DATA: join(state, "data"),
    JUPYTER_RUNTIME_DIR: join(state, "runtime"),
    JUPYTER_CONFIG_DIR: join(state, "config"),
    JUPYTER_DATA_DIR: join(state, "data"),
  };
  delete env.JUPYTER_TOKEN;
  const agent = spawn(bin, ["mcp", "--no-execute"], { cwd: root, env, stdio: ["pipe", "pipe", "inherit"] });
  const lines = createInterface({ input: agent.stdout });
  const pending = new Map();
  lines.on("line", (l) => {
    const v = JSON.parse(l);
    pending.get(v.id)?.(v);
  });
  let id = 0;
  const request = (method, params) =>
    new Promise((resolve) => {
      id += 1;
      pending.set(id, resolve);
      agent.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id, method, params })}\n`);
    });
  const tool = async (name, args) => {
    const v = await request("tools/call", { name, arguments: args });
    assert.ok(!v.error && !v.result.isError, JSON.stringify(v));
    return v.result.content;
  };
  try {
    await request("initialize", {
      protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "claude-code", version: "1" },
    });
    agent.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", method: "notifications/initialized" })}\n`);
    const input = { session_id: "e2e", cwd: root };
    const post = (name, args, response) =>
      handle("post-tool", { ...input, tool_name: name, tool_input: args, tool_response: response }, env);

    assert.match(handle("session-start", input, env), /a server is running/);
    const opened = await tool("notebook_open", { path: "j.ipynb", title: "J" });
    post(`${TOOL}notebook_open`, { path: "j.ipynb", title: "J" }, opened);
    const added = await tool("cell_add", { source: "A note." });
    post(`${TOOL}cell_add`, { source: "A note." }, added);
    // A recorded Bash step: not reported back as someone else's.
    const rec = post("Bash", { command: "echo 42 # journal" }, { stdout: "42", stderr: "" });
    assert.match(rec.hookSpecificOutput.additionalContext, /recorded this command/);
    assert.equal(handle("prompt", input, env), "");
    // A person (another connection) adds a cell.
    const person = await new Promise((resolve, reject) => {
      const p = spawn(bin, ["mcp", "call", "cell_add", JSON.stringify({ path: "j.ipynb", source: "Check Node 24.10 first?" }), "--client", "person"], { cwd: root, env });
      p.on("exit", (code) => (code === 0 ? resolve() : reject(new Error(`exit ${code}`))));
    });
    void person;
    const out = handle("prompt", input, env);
    const ctx = out.hookSpecificOutput.additionalContext;
    assert.match(ctx, /1 added, 0 edited, 0 moved, 0 deleted/);
    assert.match(ctx, /Check Node 24\.10 first\?/);
    assert.doesNotMatch(ctx, /echo 42/);
    // The prompt hook moved the mark: the agent's own read sees nothing new.
    assert.equal(handle("prompt", input, env), "");
    // The journal has edits and no run-all: stopping is blocked once.
    assert.equal(handle("stop", input, env).decision, "block");
  } finally {
    agent.stdin.end();
    await new Promise((r) => agent.on("exit", r));
  }
});
