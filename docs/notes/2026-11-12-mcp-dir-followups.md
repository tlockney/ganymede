## 2026-11-12, follow-ups to the agent's folder for `ganymede mcp` (#734)

#730 made `ganymede mcp call` send its own cwd as the connection's folder
too. The ganymede-journal hooks ran it from the hook's `cwd`, which
follows the agent's `cd` in Bash, so after a `cd sub` the journal path
`j.ipynb` meant `sub/j.ipynb`. The hooks now call from
`CLAUDE_PROJECT_DIR`, where the agent's own `ganymede mcp` started, and
fall back to the hook's `cwd` outside Claude Code.

The folder is checked at `initialize`: `..` or a backslash is refused
there, rather than failing every later call with a path the agent never
wrote. The tools' parameter docs now say paths start in the directory
`notebook_open` names. `notebook://` resources stay root-relative: their
URIs are ids the client hands back, not paths it writes.
