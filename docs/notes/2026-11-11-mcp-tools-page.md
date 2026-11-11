## 2026-11-11, MCP tools page generated from the tools (#689)

mcp.md's Tools table says what each tool does in a line; the parameters
were only in the schemas an agent gets from `tools/list`. docs/mcp-tools.md
now lists every tool with its description and a parameter table (type,
required, description), generated from `Notebooks::tool_router()` by a
test in ganymede-server (src/mcp/reference.rs), which fails when the page
differs; `UPDATE_REFERENCE=1` rewrites it. Types come from the JSON
schema: optional values drop `null`, `$ref`s resolve to the enum's
values, and a documented enum (`AddMode`) shows each value's doc comment.

Nine parameters had no doc comment (`id`, `source`, `action`, two
`timeout`s), so the page had blank cells and agents saw no description.
They have one now; the two `timeout`s name their default, 600 seconds.
A `path` without one is described on the page as on `notebook_read`.
scripts/ci-changes.sh counts the page as code, so editing it by hand runs
the test.
