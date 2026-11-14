// node --test evals/review/*.test.mjs
import assert from "node:assert/strict";
import test from "node:test";
import { GIT_READ, allowedTools, disallowedTools, issuePaths } from "./tools.mjs";

const c = { repo: "tlockney/ganymede", issue: 816 };
const allowed = allowedTools(c);
const denied = disallowedTools({ cache: "/root/.cache/ganymede-review-eval", out: "/root/out-1" });

test("the only shell a review may run is read-only git and this case's issue over REST", () => {
  const bash = allowed.filter((t) => t.startsWith("Bash("));
  assert.deepEqual(bash.filter((t) => t.startsWith("Bash(git ")), GIT_READ.map((g) => `Bash(git ${g}:*)`));
  assert.deepEqual(bash.filter((t) => !t.startsWith("Bash(git ")), [
    "Bash(gh api repos/tlockney/ganymede/issues/816)",
    "Bash(gh api repos/tlockney/ganymede/issues/816/comments)",
  ]);
  for (const t of bash) assert.ok(!t.includes("*") || t.endsWith(":*)"), `wildcard only as a git subcommand prefix: ${t}`);
  // No generic shell readers, find, git grep or issue view: they write, run commands or need GraphQL.
  for (const bad of ["find", "cat", "ls", "head", "tail", "wc", "grep", "gh issue", "gh pr", "git grep", "git push", "git commit", "git checkout", "cargo", "npm"]) {
    assert.ok(!bash.some((t) => t.startsWith(`Bash(${bad}`)), bad);
  }
  assert.deepEqual(issuePaths(c), ["repos/tlockney/ganymede/issues/816", "repos/tlockney/ganymede/issues/816/comments"]);
  assert.ok(!allowed.includes("Edit") && !allowed.includes("Write") && !allowed.includes("Bash"));
});

test("file reads are scoped to the clone", () => {
  for (const t of ["Read", "Grep", "Glob"]) {
    assert.ok(!allowed.includes(t), `${t} unscoped`);
    assert.ok(allowed.includes(`${t}(./**)`));
  }
});

test("denies cover writes, answer keys, other clones, results and the bare cache", () => {
  for (const w of ["Edit", "Write", "NotebookEdit", "WebFetch", "WebSearch"]) assert.ok(denied.includes(w));
  for (const t of ["Read", "Grep", "Glob"]) {
    for (const p of ["//mnt/**", "//home/**", "//root/.cache/ganymede-review-eval/*.git/**", "//root/out-1/**"]) assert.ok(denied.includes(`${t}(${p})`), `${t} ${p}`);
  }
  for (const f of ["--output", "-O", "--no-index"]) assert.ok(denied.some((d) => d.startsWith("Bash(git * ") && d.includes(f)), f);
  for (const f of ["Bash(git -C*)", "Bash(git --git-dir*)", "Bash(git -c*)"]) assert.ok(denied.includes(f));
});
