// node --test evals/review/*.test.mjs
import assert from "node:assert/strict";
import test from "node:test";
import { handle, mcpConfig } from "./issue-mcp.mjs";

const ctx = { repo: "tlockney/ganymede", issue: 816, fetch: (p) => `GOT ${p}` };
const call = (args, name = "issue_read") => handle({ jsonrpc: "2.0", id: 1, method: "tools/call", params: { name, arguments: args } }, ctx);

test("the server lists issue_read and answers initialize", () => {
  assert.equal(handle({ id: 1, method: "initialize", params: { protocolVersion: "2025-03-26" } }, ctx).result.serverInfo.name, "github");
  assert.deepEqual(handle({ id: 2, method: "tools/list" }, ctx).result.tools.map((t) => t.name), ["issue_read"]);
  assert.equal(handle({ method: "notifications/initialized" }, ctx), null);
  assert.equal(handle({ id: 3, method: "nope" }, ctx).error.code, -32601);
});

test("it reads the case's issue and comments and nothing else", () => {
  const a = { owner: "tlockney", repo: "ganymede", issue_number: 816 };
  assert.equal(call({ ...a, method: "get" }).result.content[0].text, "GOT repos/tlockney/ganymede/issues/816");
  assert.equal(call({ ...a, method: "get_comments" }).result.content[0].text, "GOT repos/tlockney/ganymede/issues/816/comments");
  for (const bad of [{ ...a, issue_number: 817 }, { ...a, repo: "ganymede-ui" }, { ...a, owner: "x" }, { ...a, method: "get_labels" }]) {
    assert.equal(call({ method: "get", ...bad }).result.isError, true, JSON.stringify(bad));
  }
  assert.equal(call(a, "pull_request_read").result.isError, true);
  assert.match(mcpConfig({ repo: "tlockney/ganymede", issue: 816 }), /"CASE_ISSUE":"816"/);
});
