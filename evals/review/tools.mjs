// The tool rules a review run gets (pure). A review reads the clone, the
// case's issue over REST, and runs read-only git; nothing else.

import fs from "node:fs";
import path from "node:path";

/** The git subcommands a review may run. Each rule is `git <sub>:*`; `-C`, `--git-dir` and `-c` before it don't match. */
export const GIT_READ = ["log", "show", "diff", "status", "ls-files", "cat-file", "rev-parse", "merge-base", "ls-tree", "show-ref", "describe"];

/**
 * The REST path of a case's issue, exactly. Its comments are not allowed
 * here: the `issue_read` tool serves them, only those written before the
 * case's head commit.
 */
export const issuePaths = (c) => [`repos/${c.repo}/issues/${c.issue}`];

/**
 * Allowed: reads inside the clone (the cwd), subagents and skills, read-only
 * git, and `gh api` for this case's issue only. `gh issue view` needs GraphQL,
 * which is not available to these sessions.
 */
export function allowedTools(c) {
  return [
    "Read(./**)", "Grep(./**)", "Glob(./**)", "Skill", "Agent", "Task", "mcp__github__issue_read",
    ...GIT_READ.map((g) => `Bash(git ${g}:*)`),
    ...issuePaths(c).map((p) => `Bash(gh api ${p})`),
  ];
}

/** `//` makes a rule path absolute. */
const abs = (p) => `//${path.resolve(p).replace(/^\/+/, "")}`;

/** Default place of the mounted project files (review history, answer keys). */
export const PROJECT_FILES = "/mnt/project-files";

/**
 * The paths a review must not read, from the paths this run actually uses:
 * `dirs` (whole trees) and `globs`. Never a blanket /home or /Users, which
 * would also cover the review's own clone when the cache is under $HOME.
 * - the checkout of this repo and its siblings (the parent of the checkout
 *   holds the person's other worktrees; when the scratch clones are inside
 *   that parent, each sibling is denied one by one, around the path to them),
 * - the mounted project files, when they exist,
 * - the bare clones and the logs in the cache (they hold later commits),
 * - this run's `--out` (the results).
 */
export function denyPaths({ cache, out, repoRoot, work = path.join(cache, "work"), projectFiles = PROJECT_FILES, exists = fs.existsSync, list = fs.readdirSync }) {
  const parent = path.dirname(path.resolve(repoRoot));
  const w = path.resolve(work);
  const dirs = [path.resolve(out)];
  if (w.startsWith(parent + path.sep)) dirs.push(...around(parent, w, list));
  else dirs.push(parent);
  if (exists(projectFiles)) dirs.push(path.resolve(projectFiles));
  const globs = [path.join(path.resolve(cache), "*.git"), path.join(path.resolve(cache), "*.log")];
  return { dirs, globs };
}

/** The entries of `dir` that are not on the way to `keep` (inside `dir`), and, one level down, the same for the entry that is. */
function around(dir, keep, list) {
  const out = [];
  for (const e of list(dir)) {
    const p = path.join(dir, e);
    if (keep === p) continue;
    if (keep.startsWith(p + path.sep)) out.push(...around(p, keep, list));
    else out.push(p);
  }
  return out;
}

/** Throws when `dir` (the scratch clones) is inside a denied path: every read in it would be refused. */
export function assertWorkAllowed(work, { dirs }) {
  const w = path.resolve(work);
  for (const d of dirs) {
    if (w === d || w.startsWith(d + path.sep)) throw new Error(`${w} is inside ${d}, which reviews may not read; use --cache outside it (and outside the checkout's parent directory)`);
  }
}

/**
 * Denied: writes, the web, and everything that would show an answer (see
 * denyPaths). `git` flags that write a file or read one outside the repo are
 * denied wherever they sit in the command.
 */
export function disallowedTools(deny) {
  const reads = [...deny.dirs.flatMap((d) => [abs(d), `${abs(d)}/**`]), ...deny.globs.map((g) => `${abs(g)}/**`), ...deny.globs.filter((g) => g.endsWith(".log")).map((g) => abs(g))];
  return [
    "Edit", "Write", "NotebookEdit", "WebFetch", "WebSearch",
    ...reads.flatMap((p) => [`Read(${p})`, `Grep(${p})`, `Glob(${p})`]),
    "Bash(curl:*)", "Bash(git push:*)", "Bash(git commit:*)", "Bash(git checkout:*)",
    ...["--output", "-O", "--no-index", "--exclude-from", "--exclude-per-directory", "--contents", "--ignore-revs-file", "-X"].map((f) => `Bash(git * ${f}*)`),
    "Bash(git -C*)", "Bash(git --git-dir*)", "Bash(git -c*)",
  ];
}
