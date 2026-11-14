// node --test evals/lib/claude.test.mjs
import assert from "node:assert/strict";
import path from "node:path";
import test from "node:test";
import { ALLOWED_TOOLS, PLUGIN_DIR, claudeInvocation } from "./claude.mjs";

const base = { PATH: "/usr/bin" };

test("the default command allows the standard tools and loads the plugin", () => {
  const { bin, args, env } = claudeInvocation("hello", { env: base });
  assert.equal(bin, "claude");
  assert.deepEqual(args, ["-p", "hello", "--plugin-dir", PLUGIN_DIR, "--allowedTools", ALLOWED_TOOLS.join(",")]);
  assert.equal(env.PATH, "/usr/bin");
});

test("extraAllowed adds to the allow list", () => {
  const { args } = claudeInvocation("x", { env: base, extraAllowed: ["Bash(sleep:*)"] });
  const allowed = args[args.indexOf("--allowedTools") + 1].split(",");
  assert.deepEqual(allowed, [...ALLOWED_TOOLS, "Bash(sleep:*)"]);
});

test("CLAUDE_BIN and CLAUDE_ARGS replace the command and the allow list", () => {
  const { bin, args } = claudeInvocation("x", {
    env: { ...base, CLAUDE_BIN: "/opt/claude", CLAUDE_ARGS: "--model  haiku" },
    extraAllowed: ["Bash(sleep:*)"],
  });
  assert.equal(bin, "/opt/claude");
  assert.deepEqual(args, ["-p", "x", "--plugin-dir", PLUGIN_DIR, "--model", "haiku"]);
});

test("GANYMEDE_BIN's directory goes first on PATH", () => {
  const { env } = claudeInvocation("x", { env: { ...base, GANYMEDE_BIN: "/opt/g/bin/ganymede" } });
  assert.equal(env.PATH, `/opt/g/bin${path.delimiter}/usr/bin`);
});

test("the plugin is this checkout's", () => {
  assert.match(PLUGIN_DIR, /plugins[\\/]ganymede-journal$/);
});
