// node --test evals/review/*.test.mjs
import assert from "node:assert/strict";
import test from "node:test";
import { GIT_READ, allowedTools, assertWorkAllowed, denyPaths, disallowedTools, issuePaths } from "./tools.mjs";

const c = { repo: "tlockney/ganymede", issue: 816 };
const allowed = allowedTools(c);
const deny = denyPaths({ cache: "/root/.cache/ganymede-review-eval", out: "/root/out-1", repoRoot: "/home/user/wt/ganymede-822", exists: () => true });
const denied = disallowedTools(deny);

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

test("denies cover writes, answer keys, other worktrees, results and the bare cache", () => {
  for (const w of ["Edit", "Write", "NotebookEdit", "WebFetch", "WebSearch"]) assert.ok(denied.includes(w));
  for (const t of ["Read", "Grep", "Glob"]) {
    for (const p of ["//mnt/project-files/**", "//home/user/wt/**", "//root/.cache/ganymede-review-eval/*.git/**", "//root/.cache/ganymede-review-eval/*.log", "//root/out-1/**"]) assert.ok(denied.includes(`${t}(${p})`), `${t} ${p}`);
  }
  for (const f of ["--output", "-O", "--no-index", "--exclude-from", "--contents", "--ignore-revs-file", "-X"]) assert.ok(denied.includes(`Bash(git * ${f}*)`), f);
  for (const f of ["Bash(git -C*)", "Bash(git --git-dir*)", "Bash(git -c*)"]) assert.ok(denied.includes(f));
  assert.ok(!allowed.includes("Bash(git blame:*)"), "blame reads files named by --contents");
});

test("no blanket /home or /Users deny: the clone under $HOME stays readable (Linux user and macOS)", () => {
  const cases = [
    { home: "/home/alice", repo: "/home/alice/wt/ganymede-822" },
    { home: "/Users/bob", repo: "/Users/bob/code/ganymede" },
  ];
  for (const { home, repo } of cases) {
    const cache = `${home}/.cache/ganymede-review-eval`;
    const d = denyPaths({ cache, out: `${home}/out`, repoRoot: repo, exists: (p) => p === "/mnt/project-files" });
    const rules = disallowedTools(d);
    for (const bad of ["//home/**", "//Users/**", `/${home}/**`, `//${home.slice(1)}/**`]) assert.ok(!rules.some((r) => r.endsWith(`(${bad})`)), bad);
    assert.doesNotThrow(() => assertWorkAllowed(`${cache}/work/x-1-1`, d), home);
    // the person's other worktrees are denied, wherever they are
    assert.ok(rules.includes(`Read(//${repo.slice(1, repo.lastIndexOf("/"))}/**)`), repo);
  }
  const d = denyPaths({ cache: "/home/alice/.cache/e", out: "/o", repoRoot: "/x/y", exists: () => false });
  assert.ok(!disallowedTools(d).some((r) => r.includes("project-files")), "only when the mount exists");
});

test("fail fast when the scratch clones would sit inside a denied path", () => {
  // The checkout is /home/alice/ganymede, so its parent is $HOME, and the cache is under it.
  const d = denyPaths({ cache: "/home/alice/.cache/e", out: "/home/alice/out", repoRoot: "/home/alice/ganymede", exists: () => false });
  assert.throws(() => assertWorkAllowed("/home/alice/.cache/e/work", d), /may not read/);
  assert.doesNotThrow(() => assertWorkAllowed("/var/cache/e/work", d));
});
