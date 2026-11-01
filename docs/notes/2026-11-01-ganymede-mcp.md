## 2026-11-01, `ganymede mcp` (#245)

Notebook tools for coding agents over MCP, in Rust with `rmcp` 3.8
(Apache-2.0; its new dependencies are MIT or Apache-2.0). Every other
notebook MCP server needs Python.

- **One server, two transports.** The tools live in
  `ganymede_server::mcp` and are served at `/mcp` (streamable HTTP,
  behind the API's auth; rmcp's own Host check is off because the
  server's `allow_remote_access` check already runs). `ganymede mcp` on
  stdio is a proxy to some server's `/mcp`: `--server`, else a running
  `serve --mcp` whose root holds the cwd (from `jpserver-*.json`; the
  probe is a session-less `GET /mcp`, which rmcp refuses with something
  other than the 404 of a server without it), else `run_with` in the
  same process on port 0 with `Startup::stop`, so nothing goes to
  stdout or the runtime dir. The issue suggested a background
  `serve --port 0` with idle culling; in-process ties the server's life
  to the agent's connection and needs no culling.
- **Proxy.** One thread per message so a long `cell_run` doesn't block a
  cancellation; `initialize` alone runs inline, since it sets the
  `Mcp-Session-Id` the rest need. Answers come back as SSE even for one
  message in rmcp's session mode (`json_response` applies only to
  stateless mode); sessions are kept because the client's name, from
  `initialize`, is the provenance `by`.
- **Rooms, not files.** Reads and edits run as closures on the
  notebook's room task (`Cmd::With`), and recorded runs go through the
  #244 execution queue (cells without a hash check), so a person
  watching in JupyterLab or ganymede-ui sees the agent's cells and
  outputs live. Scratch runs (`record: false`) use the kernel directly
  and touch nothing.
- **Saving on stop.** A room saves 1 s after its last edit, and stopping
  the server dropped edits younger than that. `Collab::flush_all` saves
  every dirty room after the listener closes; `ganymede mcp` waits for
  its server to stop before exiting. This fixes `ganymede serve` too.
- **rmcp quirks.** A request without `Host` is a 400 even with the
  allowed-hosts check off; real clients always send one.
- **`--read-only`, `--no-execute`, `--images`** can't be passed to a
  running server per connection, so they make `ganymede mcp` start its
  own server.
