## 2026-11-18, journal notebooks stand alone (#863)

The journal skill told agents to put code several cells share in a
module next to the notebook (a `.ts` file the kernel imports). The worked
examples did that (`quakes.ts`, `fremont.ts`), and each journal then
needed a second file to re-run, review or share; the published page
showed calls into code it didn't contain.

The skill now keeps that code in helper cells near the top of the
notebook, each starting with a comment that says what it defines. A
person can collapse them, and the published page folds all code anyway.
A module is still right for code another notebook imports too, or code
too long to read as a cell, and the agent says why in a note. The
journal reviewer flags a local module only one notebook imports.

The agent can't collapse a cell itself: no tool sets
`metadata.jupyter.source_hidden`. The existing examples under
`examples/journals/` keep their modules; they record how those runs went.
