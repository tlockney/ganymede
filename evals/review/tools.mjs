// The tool rules a review run gets (pure). A review reads the clone, the
// case's issue over REST, and runs read-only git; nothing else.

import path from "node:path";

/** The git subcommands a review may run. Each rule is `git <sub>:*`; `-C`, `--git-dir` and `-c` before it don't match. */
export const GIT_READ = ["log", "show", "diff", "status", "blame", "ls-files", "cat-file", "rev-parse", "merge-base", "ls-tree", "show-ref", "describe"];

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

/**
 * Denied: writes, the web, and everything that would show an answer: the
 * mounted project files (reviews, research), other people's clones under
 * /home, this eval's output and the bare clones, which hold later commits.
 * `git` flags that write a file or read outside the repo are denied wherever
 * they sit in the command.
 */
export function disallowedTools({ cache, out }) {
  const reads = ["//mnt/**", "//home/**", `${abs(cache)}/*.git/**`, `${abs(cache)}/*.log`, `${abs(out)}/**`];
  return [
    "Edit", "Write", "NotebookEdit", "WebFetch", "WebSearch",
    ...reads.flatMap((p) => [`Read(${p})`, `Grep(${p})`, `Glob(${p})`]),
    "Bash(curl:*)", "Bash(git push:*)", "Bash(git commit:*)", "Bash(git checkout:*)",
    "Bash(git * --output*)", "Bash(git * -O*)", "Bash(git * --no-index*)", "Bash(git -C*)", "Bash(git --git-dir*)", "Bash(git -c*)",
  ];
}
