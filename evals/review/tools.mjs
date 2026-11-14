// The tool rules a review run gets (pure). A review reads the clone, the
// case's issue over REST, and runs read-only git; nothing else.

import fs from "node:fs";
import path from "node:path";

/** The git subcommands a review may run. Each rule is `git <sub>:*`; `-C`, `--git-dir` and `-c` before it don't match. */
export const GIT_READ = ["log", "show", "diff", "status", "ls-files", "cat-file", "rev-parse", "merge-base", "ls-tree", "show-ref", "describe"];

/** The REST paths of a case's issue: the issue and its comments, exactly. */
export const issuePaths = (c) => [`repos/${c.repo}/issues/${c.issue}`, `repos/${c.repo}/issues/${c.issue}/comments`];

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
 *   holds the person's other worktrees),
 * - the mounted project files, when they exist,
 * - the bare clones and the logs in the cache (they hold later commits),
 * - this run's `--out` (the results).
 */
export function denyPaths({ cache, out, repoRoot, projectFiles = PROJECT_FILES, exists = fs.existsSync }) {
  const dirs = [path.dirname(path.resolve(repoRoot)), path.resolve(out)];
  if (exists(projectFiles)) dirs.push(path.resolve(projectFiles));
  const globs = [path.join(path.resolve(cache), "*.git"), path.join(path.resolve(cache), "*.log")];
  return { dirs, globs };
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
  const reads = [...deny.dirs.map((d) => `${abs(d)}/**`), ...deny.globs.map((g) => `${abs(g)}/**`), ...deny.globs.filter((g) => g.endsWith(".log")).map((g) => abs(g))];
  return [
    "Edit", "Write", "NotebookEdit", "WebFetch", "WebSearch",
    ...reads.flatMap((p) => [`Read(${p})`, `Grep(${p})`, `Glob(${p})`]),
    "Bash(curl:*)", "Bash(git push:*)", "Bash(git commit:*)", "Bash(git checkout:*)",
    ...["--output", "-O", "--no-index", "--exclude-from", "--exclude-per-directory", "--contents", "--ignore-revs-file", "-X"].map((f) => `Bash(git * ${f}*)`),
    "Bash(git -C*)", "Bash(git --git-dir*)", "Bash(git -c*)",
  ];
}
