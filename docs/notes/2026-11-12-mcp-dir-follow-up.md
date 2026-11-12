## 2026-11-12, journal hooks and tool docs follow the connection's folder (#732)

A review of #730 that arrived after it merged found places still written
for root-relative paths.

- The journal hooks ran `ganymede mcp call` from the hook's current
  folder, which follows the agent's `cd` in Bash. `ganymede mcp call`
  now sends that folder as the connection's directory, so after `cd sub`
  the hooks wrote to `sub/journal/…`. The hook state keeps the session's
  start folder (`home`) and every call runs from it. A test changes folder
  between the open and the next prompt.
- Tool argument and description text says "relative to this connection's
  directory" and how a leading `/` reads; docs/mcp-tools.md is
  regenerated. docs/mcp.md's opening line no longer contradicts the
  section below it.
- `notebook_publish` errors, resource names (`list_resources`) and
  `read_resource` use the connection's paths, so a name in an answer reads
  back to the same file.
- `initialize` rejects a `_meta["ganymede/dir"]` with `..` or `\`, naming
  the bad folder, rather than failing each later call with an outside-root
  error about the agent's path.
