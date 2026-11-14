#!/usr/bin/env node
// Runs the person-and-agent evals (#824) and writes a report.
//
//   node evals/agent-native/run.mjs [--runs N] [--scenario ID] [--out DIR]
//   node evals/agent-native/run.mjs --rescore DIR
//
// Each run starts `ganymede serve --mcp` on a temporary root with its own
// config, data and runtime dirs, then at once runs
//   - `claude -p` with the ganymede-journal plugin, in that root (its
//     `ganymede mcp` finds the server by the server info file), and
//   - person.py, a room client that edits the same notebook as the
//     scenario's steps say, interleaved with the agent's edits.
// When the agent exits the person is stopped, the server stops (saving
// the notebook), and the notebook it left is scored: file state only.
//
// `ganymede` comes from GANYMEDE_BIN or PATH; CLAUDE_BIN and CLAUDE_ARGS
// replace the claude command; EVAL_PYTHON is the Python with jupyter_ydoc,
// pycrdt and websockets (default python3); EVAL_TIMEOUT_MS bounds each
// agent session (default 15 minutes); EVAL_PERSON_GRACE_MS (default 30
// seconds) is how long the person may keep going after the agent exits. A session that exits non-zero or
// times out, or a person who couldn't connect, is reported and left out
// of the pass rate.

import { spawn } from "node:child_process";
import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { claudeInvocation, evalTimeoutMs } from "../lib/claude.mjs";
import { NOTEBOOK, SCENARIOS, notebookFor } from "./scenarios.mjs";
import { reportMarkdown, score, summarize } from "./score.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));

function args(argv) {
  const o = { runs: 1, scenario: null, out: null, rescore: null };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--runs") o.runs = Number(argv[++i]);
    else if (a === "--scenario") o.scenario = argv[++i];
    else if (a === "--out") o.out = argv[++i];
    else if (a === "--rescore") o.rescore = argv[++i];
    else throw new Error(`unknown argument: ${a}`);
  }
  return o;
}

/** Children to stop if the runner is interrupted, so no server or session is left running. */
const live = new Set();
for (const sig of ["SIGINT", "SIGTERM"]) {
  process.on(sig, () => {
    for (const c of live) c.kill("SIGKILL");
    process.exit(130);
  });
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const readJson = (p) => JSON.parse(fs.readFileSync(p, "utf8"));

function freePort() {
  return new Promise((resolve, reject) => {
    const s = net.createServer();
    s.once("error", reject);
    s.listen(0, "127.0.0.1", () => {
      const { port } = s.address();
      s.close(() => resolve(port));
    });
  });
}

/** Waits for a child to exit; resolves `{code, signal}`. */
function exited(child) {
  return new Promise((resolve) => {
    if (child.exitCode !== null || child.signalCode !== null) resolve({ code: child.exitCode, signal: child.signalCode });
    else child.once("exit", (code, signal) => resolve({ code, signal }));
  });
}

async function stop(child, ms = 20000) {
  if (child.exitCode !== null || child.signalCode !== null) return;
  child.kill("SIGTERM");
  const timer = setTimeout(() => child.kill("SIGKILL"), ms);
  await exited(child);
  clearTimeout(timer);
}

/** A scenario's server: a temp root and dirs, `ganymede serve --mcp` on a free port. */
async function startServer(scenario, base, log) {
  const root = path.join(base, "root");
  const dirs = Object.fromEntries(
    ["config", "jupyter-config", "data", "runtime"].map((d) => [d, path.join(base, d)]),
  );
  for (const d of [root, ...Object.values(dirs)]) fs.mkdirSync(d, { recursive: true });
  fs.writeFileSync(path.join(root, NOTEBOOK), notebookFor(scenario));
  const port = await freePort();
  const token = `eval-${Math.random().toString(36).slice(2)}${Date.now().toString(36)}`;
  const toml = [
    `root_dir = ${JSON.stringify(root)}`,
    `ip = "127.0.0.1"`,
    `port = ${port}`,
    `token = ${JSON.stringify(token)}`,
    "mcp_enabled = true",
    // No kernel: the scenarios hold Markdown only.
    "mcp_no_execute = true",
    ...Object.entries(scenario.server ?? {}).map(([k, v]) => `${k} = ${JSON.stringify(v)}`),
  ].join("\n");
  fs.writeFileSync(path.join(dirs.config, "ganymede.toml"), toml + "\n");
  const env = {
    ...process.env,
    GANYMEDE_CONFIG_DIR: dirs.config,
    JUPYTER_CONFIG_DIR: dirs["jupyter-config"],
    JUPYTER_DATA_DIR: dirs.data,
    JUPYTER_RUNTIME_DIR: dirs.runtime,
  };
  const bin = process.env.GANYMEDE_BIN ?? "ganymede";
  const logFd = fs.openSync(log, "w");
  const child = spawn(bin, ["serve"], { cwd: root, env, stdio: ["ignore", logFd, logFd] });
  child.on("error", () => {});
  live.add(child);
  const url = `http://127.0.0.1:${port}`;
  for (let i = 0; i < 150; i++) {
    if (child.exitCode !== null) throw new Error(`ganymede serve exited ${child.exitCode}; see ${log}`);
    try {
      const r = await fetch(`${url}/api/status`, { headers: { Authorization: `token ${token}` } });
      if (r.ok) return { child, url, token, root, env };
    } catch {}
    await sleep(200);
  }
  await stop(child);
  throw new Error(`ganymede serve didn't answer; see ${log}`);
}

/** One run of one scenario: `{run, scenario, score | error}`. */
async function runOne(scenario, run, out) {
  const base = fs.mkdtempSync(path.join(os.tmpdir(), `agent-native-${run}-`));
  console.error(`${run}: ${base}`);
  const result = { run, scenario: scenario.id, score: null };
  let server;
  let person;
  try {
    server = await startServer(scenario, base, path.join(out, `${run}.server.log`));
    const script = path.join(base, "steps.json");
    const personOut = path.join(base, "person.json");
    fs.writeFileSync(script, JSON.stringify(scenario.person));
    person = spawn(
      process.env.EVAL_PYTHON ?? "python3",
      [
        path.join(here, "person.py"),
        "--server", server.url, "--token", server.token,
        "--path", NOTEBOOK, "--script", script, "--out", personOut,
      ],
      { stdio: ["ignore", "inherit", "inherit"], env: { ...process.env, PYTHONDONTWRITEBYTECODE: "1" } },
    );
    const personDone = exited(person);
    person.on("error", () => {});
    live.add(person);
    // The agent starts once the person is in the room, so the notebook is open.
    for (let i = 0; i < 150 && !(fs.existsSync(personOut) && readJson(personOut).connected); i++) {
      if (person.exitCode !== null) break;
      await sleep(200);
    }
    if (person.exitCode !== null) throw new Error("the simulated person exited before the agent started");

    const { bin, args: cargs, env } = claudeInvocation(scenario.prompt, {
      env: { ...server.env, GANYMEDE_JOURNAL_STOP: "off" },
      extraAllowed: ["Bash(sleep:*)"],
    });
    const agent = spawn(bin, cargs, { cwd: server.root, env, stdio: ["ignore", "pipe", "inherit"] });
    live.add(agent);
    let log = "";
    agent.stdout.on("data", (d) => (log += d));
    let timedOut = false;
    const timer = setTimeout(() => {
      timedOut = true;
      agent.kill("SIGKILL");
    }, evalTimeoutMs(15 * 60 * 1000));
    const agentExit = await new Promise((resolve) => {
      agent.on("error", (e) => resolve({ code: null, signal: String(e) }));
      agent.on("exit", (code, signal) => resolve({ code, signal }));
    });
    clearTimeout(timer);
    fs.writeFileSync(path.join(out, `${run}.log`), log);
    const failed = timedOut ? "timed out" : agentExit.code !== 0 ? `exited ${agentExit.code ?? agentExit.signal}` : null;

    // Give the person a moment to finish steps the agent didn't wait for.
    const grace = Number(process.env.EVAL_PERSON_GRACE_MS ?? 30000);
    const waiting = new AbortController();
    const timer2 = setTimeout(() => waiting.abort(), grace);
    await waitSteps(personOut, scenario.person.length, waiting.signal, person);
    clearTimeout(timer2);
    await sleep(1000);
    await stop(person);
    await personDone;
    // The room saves about a second after the last edit; stopping the server saves too.
    await sleep(2500);
    await stop(server.child);
    const nbPath = path.join(server.root, NOTEBOOK);
    fs.copyFileSync(nbPath, path.join(out, `${run}.ipynb`));
    const log2 = fs.existsSync(personOut) ? readJson(personOut) : null;
    if (log2) fs.writeFileSync(path.join(out, `${run}.person.json`), JSON.stringify(log2, null, 2) + "\n");
    if (failed) result.error = `agent ${failed}`;
    else if (!log2) result.error = "no log from the simulated person";
    else if (!log2.connected) result.error = `the simulated person couldn't connect: ${log2.error}`;
    else result.score = score(scenario, readJson(path.join(out, `${run}.ipynb`)), log2);
  } catch (e) {
    result.error = String(e.message ?? e);
  } finally {
    if (person) await stop(person).catch(() => {});
    if (server) await stop(server.child).catch(() => {});
  }
  return result;
}

/**
 * Resolves once person.py's log (which it rewrites after each step) shows
 * every step done or one that didn't finish, or when `signal` aborts (the
 * grace is over) or `person` has exited with nothing more to write.
 */
async function waitSteps(file, n, signal, person) {
  while (!signal.aborted) {
    try {
      const { steps } = readJson(file);
      if (steps.length >= n && steps.every((s) => s.status === "done")) return;
      if (steps.some((s) => s.status === "timeout" || s.status === "error")) return;
    } catch {}
    if (person.exitCode !== null || person.signalCode !== null) return;
    await sleep(500);
  }
}

function finish(results, out) {
  fs.writeFileSync(path.join(out, "results.json"), JSON.stringify(results, null, 2) + "\n");
  fs.writeFileSync(path.join(out, "report.md"), reportMarkdown(results));
  for (const r of results) console.log(`${r.run}: ${r.error ? `excluded (${r.error})` : r.score.pass ? "pass" : `FAIL ${r.score.reasons.join("; ")}`}`);
  console.log(JSON.stringify(summarize(results), null, 2));
  console.error(`results: ${out}`);
}

export function rescore(out) {
  const results = readJson(path.join(out, "results.json")).map((r) => {
    const scenario = SCENARIOS.find((s) => s.id === r.scenario);
    const nb = path.join(out, `${r.run}.ipynb`);
    const person = path.join(out, `${r.run}.person.json`);
    if (r.error || !scenario || !fs.existsSync(nb)) return { ...r, score: null };
    return { ...r, score: score(scenario, readJson(nb), fs.existsSync(person) ? readJson(person) : null) };
  });
  finish(results, out);
  return results;
}

async function main() {
  const o = args(process.argv.slice(2));
  if (o.rescore) return rescore(o.rescore);
  const scenarios = o.scenario ? SCENARIOS.filter((s) => s.id === o.scenario) : SCENARIOS;
  if (!scenarios.length) throw new Error(`no scenario ${o.scenario}`);
  const out = o.out ?? fs.mkdtempSync(path.join(os.tmpdir(), "agent-native-evals-"));
  fs.mkdirSync(out, { recursive: true });
  const results = [];
  for (const s of scenarios) {
    for (let n = 1; n <= o.runs; n++) {
      results.push(await runOne(s, `${s.id}-${n}`, out));
      fs.writeFileSync(path.join(out, "results.json"), JSON.stringify(results, null, 2) + "\n");
    }
  }
  finish(results, out);
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) await main();
