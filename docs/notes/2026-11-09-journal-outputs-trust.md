## 2026-11-09, journal notebooks open trusted (#597)

Thomas journaled a few notebooks with Claude Code and saw no outputs in
the app. The outputs were in the files. The notebooks were unsigned, so
the app treated them as untrusted: the p5.js canvases (HTML iframes)
were sanitized down to their captions, and an SVG chart with no
text fallback showed nothing.

A room saves through the contents manager like a frontend, and the
contents manager signs only when every code cell arrives marked
`trusted`. The room never marked any. Now it marks each code cell
trusted unless its outputs are among those it read from an unsigned
file, at load or when it merged a file change in. The room keeps those
as a set of serialized output lists, not cell ids, because a 4.4
notebook's cell ids are made up when the room opens it, and matching by
content fails closed: outputs identical to an untrusted file's stay
untrusted wherever they move. The set only empties when a save signs
the file.

Outputs a client writes into the room count as trusted, as they would if
that client saved through `PUT /api/contents` with its cells marked
trusted: both need the server's token.

The second half of the ask: keeping outputs stays the default, and
`keep_outputs: false` / `cell_clear_outputs` let an agent keep a cell's
code but leave its outputs out. The mark sits in
`metadata.ganymede.outputs` so `notebook_run_all` and later edits honour
it. It clears outputs after the run, so it is not a way to hide
credentials; the skill says not to print them at all.
