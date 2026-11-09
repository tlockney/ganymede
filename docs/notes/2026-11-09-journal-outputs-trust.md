## 2026-11-09, journal notebooks open trusted (#597)

Thomas journaled a few notebooks with Claude Code and saw no outputs in
the app. The outputs were in the files. The notebooks were unsigned, so
the app treated them as untrusted: the p5.js canvases (HTML iframes)
were sanitized down to their captions, and an SVG chart with no
text fallback showed nothing.

A room saves through the contents manager like a frontend, and the
contents manager signs only when every code cell arrives marked
`trusted`. The room never marked any. Now it marks each code cell
trusted unless it holds outputs the room read from an unsigned file, at
load or when it merged a file change in. The room keeps, per room cell
id, digests of those outputs; a run through the execute path (which MCP
uses) drops the cell's entry, so rerunning an old journal repairs it
even when the outputs come out the same. Trusting the file with the
room open (`ganymede trust`, a frontend's Trust) is seen at the next
save, which finds the file signed and forgets the entries.

The first version keyed by serialized output text and failed open: the
room gives output objects back with their keys in another order (Yjs
maps are hash maps), so nothing matched and untrusted HTML got signed.
Digests now hash a canonical form (keys sorted) of the outputs as the
room holds them, with the file passed through a throwaway room first so
numbers match too. Matching per output, not per list, keeps a cell
untrusted when someone removes only one of its outputs.

Outputs a client writes into the room count as trusted, as they would if
that client saved through `PUT /api/contents` with its cells marked
trusted: both need the server's token.

The second half of the ask: keeping outputs stays the default, and
`keep_outputs: false` / `cell_clear_outputs` let an agent keep a cell's
code but leave its outputs out. The mark sits in
`metadata.ganymede.outputs` so `notebook_run_all` and later edits honour
it. It clears outputs after the run, so it is not a way to hide
credentials; the skill says not to print them at all.
