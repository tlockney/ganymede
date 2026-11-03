## 2026-11-03, One `strip_ansi` (#295)

There were three copies of `strip_ansi`: the CLI's renderer, the CLI's
kernel tests, and `ganymede mcp`. The last two ended a CSI sequence only
on an ASCII letter, so a sequence ending in `~` (such as bracketed paste,
`ESC [ 200 ~`) swallowed the text after it. They also left OSC sequences
such as OSC 8 hyperlinks in what the agent read.

The renderer's version was already right: a CSI ends on any final byte
in `0x40..=0x7E`, and an OSC ends on BEL or ST (`ESC \`). It now lives in
`ganymede-nbformat` as `strip_ansi`, since output text is nbformat's
domain and both the CLI and the server depend on that crate. `mcp.rs`
calls it by full path, which keeps its import lines unchanged for the
open PRs that touch that file.
