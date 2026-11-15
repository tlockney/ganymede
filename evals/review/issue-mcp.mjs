#!/usr/bin/env node
// A stdio MCP server named `github` with one tool, `issue_read`, which the
// review prompt names ("GitHub MCP `issue_read`") and a headless session
// doesn't have. It serves only the case's own issue and its comments (read
// over REST with `gh api`), so a review can read the issue as it would in
// life and nothing else on GitHub. CASE_REPO and CASE_ISSUE say which issue.
// CASE_BEFORE (the case's head commit date) drops comments written after it,
// so a review doesn't read the discussion of its own fix. The issue body is
// read live: GitHub keeps no earlier version of it here.

import { execFileSync } from "node:child_process";
import readline from "node:readline";
import { fileURLToPath } from "node:url";
import path from "node:path";

export const TOOL = {
  name: "issue_read",
  description: "Get details of a GitHub issue: method get returns the issue, get_comments its comments.",
  inputSchema: {
    type: "object",
    properties: {
      method: { type: "string", enum: ["get", "get_comments"] },
      owner: { type: "string" },
      repo: { type: "string" },
      issue_number: { type: "number" },
    },
    required: ["method", "owner", "repo", "issue_number"],
  },
};

const ghGet = (p) => execFileSync("gh", ["api", p], { encoding: "utf8", maxBuffer: 32 * 1024 * 1024 });

/** The reply to one JSON-RPC request, or null for a notification. `fetch` reads a REST path. */
export function handle(req, { repo, issue, before = null, fetch = ghGet }) {
  const ok = (result) => ({ jsonrpc: "2.0", id: req.id, result });
  const err = (code, message) => ({ jsonrpc: "2.0", id: req.id, error: { code, message } });
  if (req.id === undefined) return null;
  if (req.method === "initialize") {
    return ok({ protocolVersion: req.params?.protocolVersion ?? "2024-11-05", capabilities: { tools: {} }, serverInfo: { name: "github", version: "1.0.0" } });
  }
  if (req.method === "tools/list") return ok({ tools: [TOOL] });
  if (req.method === "tools/call") {
    const a = req.params?.arguments ?? {};
    const text = (t, isError = false) => ok({ content: [{ type: "text", text: t }], isError });
    if (req.params?.name !== TOOL.name) return text(`unknown tool ${req.params?.name}`, true);
    if (`${a.owner}/${a.repo}` !== repo || Number(a.issue_number) !== issue) return text(`only ${repo}#${issue} can be read in this session`, true);
    if (a.method !== "get" && a.method !== "get_comments") return text(`method ${a.method} is not available`, true);
    try {
      const body = fetch(`repos/${repo}/issues/${issue}${a.method === "get_comments" ? "/comments" : ""}`);
      return text(a.method === "get_comments" && before ? beforeDate(body, before) : body);
    } catch (e) {
      return text(`could not read the issue: ${e.message}`, true);
    }
  }
  return err(-32601, `method not found: ${req.method}`);
}

/** Comments (a JSON array) written at or before `before` (an ISO date). */
export function beforeDate(body, before) {
  const t = Date.parse(before);
  return JSON.stringify(JSON.parse(body).filter((c) => Date.parse(c.created_at) <= t));
}

/** The `--mcp-config` JSON for a case; `before` is its head commit's date. */
export function mcpConfig(c, before = "") {
  return JSON.stringify({ mcpServers: { github: { command: process.execPath, args: [fileURLToPath(import.meta.url)], env: { CASE_REPO: c.repo, CASE_ISSUE: String(c.issue), CASE_BEFORE: before } } } });
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const ctx = { repo: process.env.CASE_REPO, issue: Number(process.env.CASE_ISSUE), before: process.env.CASE_BEFORE || null };
  readline.createInterface({ input: process.stdin }).on("line", (line) => {
    let req;
    try { req = JSON.parse(line); } catch { return; }
    const res = handle(req, ctx);
    if (res) process.stdout.write(JSON.stringify(res) + "\n");
  });
}
