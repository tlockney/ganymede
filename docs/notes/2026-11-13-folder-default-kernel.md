## 2026-11-13, a folder's default kernel (#739)

Thomas wanted an easy way to set which kernel a journal uses in
different contexts, other than a line in CLAUDE.md. He chose a
per-folder setting the skill defers to, with the skill inferring a
kernel only where none is set.

The setting is `kernel` in a `.ganymede.toml`, the nearest from the
notebook's folder up to the server root. It lives in the server, not
the Claude Code plugin, so any MCP client gets it, and the apps' New
Notebook could use it later. `notebook_open` uses it for a new notebook
called without `kernel` and says so in its reply; `kernelspecs_list`
reports it, which the plugin's start hook already calls, so the agent
sees it at the start of a session.

Choices:

- Read from disk and only under the root. The contents API hides
  dotfiles, and a file above the root could belong to anyone.
- A symlink is refused rather than followed: a TOML parse error quotes
  the file, which would leak what the link points at.
- Unknown keys and a kernel that isn't installed are errors naming the
  file, so a typo shows up instead of a silent fallback.
- A file without `kernel` passes the choice up, so a later key in a
  subfolder's file doesn't hide the parent's kernel.
