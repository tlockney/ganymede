## 2026-11-09, notebooks_search (#624)

The notebooks-from-notebooks research
(/mnt/project-files/research/2026-11-09-notebooks-from-notebooks.md)
found that nothing searches across a collection of notebooks, and that
"what do I already know about X" is the first thing Journal needs from
it. `notebooks_search` is the agent side of that: every word of a query,
ignoring case, over cell sources and optionally text outputs, under a
directory.

A notebook open in a room is read through `Collab::open_notebook`, which
looks the file id up without making one (`FileIds::get`) and reads only a
room that is already open; others are read from disk through the contents
API. So a search opens no rooms, takes no room locks, and adds no rows to
the file id table. The walk goes through the contents API too, so hidden
files stay out as they do for the rest of the API, and stops at 2,000
notebooks.

Plain substring matching, no index: a journal directory is small, and an
index (a new dependency such as tantivy) can wait until a search is slow.
A "finding" cell tag, which would let search return only findings, is
an open question for Thomas, as is an in-kernel library.

The journal skill and the `journal` prompt now search before starting a
new journal.
