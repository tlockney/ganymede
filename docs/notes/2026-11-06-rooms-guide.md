## 2026-11-06, A guide page for document rooms (#410)

Document rooms were described only in `protocol.md`, a requirements
checklist under "How it works", so a reader who wanted live editing, or
an agent author who wanted the execute endpoint, landed in the middle of
a design note. `rooms.md` is now the Guide page for them, and
`protocol.md` keeps a design paragraph that links it.

`protocol.md`, `ui.md` and `ipados.md` moved to Background as design
notes. Status in them went stale twice (the 2026-10-19 and 2026-11-06
reviews), so they now point at Known gaps and the Roadmap for status.
`ipados.md` lost "What is left": the native on-device kernel is on the
Roadmap, and `tailscale serve` identity goes there in the roadmap
rewrite.
