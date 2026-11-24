## 2026-11-24, Skills over MCP (#936)

`ganymede mcp` now serves the journal plugin's two skills, `journal` and
`page-style`, over the MCP Skills extension (`io.modelcontextprotocol/skills`,
SEP-2640), so Claude Code can load them from the server without the plugin.

- **The catalog.** `ganymede-server/src/mcp/skills.rs` embeds the files of
  `plugins/ganymede-journal/skills` with `include_str!`: one source, no
  copies, nothing read at run time. The server declares the extension in
  `get_info`, answers `skills/list` and `skills/get` (an unknown URI is
  -32602), and serves each `skill://<name>/<file>` through `resources/read`.
  The manifest has each file's sha256 digest and size. The frontmatter is
  parsed from `SKILL.md` as plain `key: value` lines; a test fails on
  anything richer, and another fails when the plugin has a file the catalog
  doesn't.
- **What Claude Code does.** 2.1.331 opens a stdio server with
  `server/discover` (protocol 2026-07-28), sends no `initialize`, and puts
  who it is in `_meta` on every request. It sends `skills/list` and
  `skills/get` only on such a connection, and only to a server that declares
  the extension. Until now `ganymede mcp` forwarded `server/discover`
  without the headers a 2026-07-28 request needs, the server answered
  -32020, and Claude Code fell back to `initialize`, so it never asked for
  skills.
- **The proxy.** `server/discover` goes to the server with
  `MCP-Protocol-Version` and `Mcp-Method`, with no session, and its answer
  is written before the next line is read. The first other message that
  carries the 2026-07-28 `_meta` makes the proxy start a session with an
  `initialize` built from that `_meta` (client info and capabilities), kept
  as the session's `init`, so a session the server ends is replaced as
  before, and the folder of a server rooted above the agent's goes in it.
  The message itself goes in that session with the three handshake keys
  taken out of its `_meta`. Clients that send `initialize` are unchanged.
- **Why not forward the 2026-07-28 messages as they are.** The server
  (rmcp) serves any request that names its protocol version without a
  session, whatever `Mcp-Session-Id` it carries. A notebook opened by one
  call is then not open for the next, the client's name is not the one in
  provenance, and the agent's folder is lost. Taking the keys out puts the
  message in the session, with state; the answers have the 2025-11-25
  shape, and Claude Code fills in the `resultType`, `ttlMs` and
  `cacheScope` a 2026-07-28 result has (it logs that it did).
- **Opt-out.** `GANYMEDE_MCP_SKILLS=off` makes the proxy answer
  `server/discover` itself with -32601, so Claude Code falls back to
  `initialize`; its answer is relayed without the extension, and `skills/list`,
  `skills/get` and `skill://` reads are refused. The plugin sets it in its `.mcp.json`,
  since it ships the skills itself and they would be listed twice; older
  `ganymede` releases ignore the variable. `ganymede mcp call` and `watch`
  stay on `initialize`.
- **Follow-up ([#937](https://github.com/tlockney/ganymede/issues/937)).** A client that posts 2026-07-28 requests straight to
  `ganymede serve --mcp` (no `ganymede mcp`) is served statelessly: each
  request gets a fresh connection, so path-less tool calls fail and the
  client's folder isn't used. Fixing it means holding a connection's state
  across such requests on the server.
