## 2026-11-11, host API design note (#647)

Cells can ask the app for things through `device` but can't ask the
server for anything, which blocks reading, searching and running other
notebooks from a cell. Thomas chose, in the direction review, a `host`
API over a comm the server opens rather than a server token in the
kernel. This note is the design for review before any code:
[A host API for kernels](../host-api.md).

It proposes the comm (`ganymede.host.v1`, opened on a kernel subshell
at start, calls on iopub, replies on shell), five `notebooks.*` methods,
per-notebook grants kept by the server, and a notebooks core shared with
the MCP tools in a new `ganymede-host` crate that `ganymede-engine` uses.
Reading the code for it showed two things. Because the server already
fans out each kernel's iopub, it can keep the host comm's messages from
every client, so the `device` comm's announce step and pings aren't
needed. And the MCP tools return nb-cli text, so they need splitting
into a core that returns data before a second front door can share them;
`mcp/search.rs` already has that shape.

Thomas agreed the design the same day and settled its four open
questions: reads cover the whole root by default, grants are kept by file
id with `ganymede grant --list` and `--revoke`, kernels without subshells
get no host API, and a notebook may edit its own cells (`self.*`) as the
step after the read calls.
