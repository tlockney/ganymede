## 2026-11-09, cell_snapshot follow-ups (#618)

A second review of #613 found five small gaps in `cell_snapshot`. A long
Markdown output cut at 4,000 characters could end inside a code fence
and turn the rest of the pasted snapshot into code; a cut one now goes
in a text fence instead. The notebook name went into file names as is,
so `Q#3 results.ipynb` gave a link a renderer reads as a fragment: names
keep `[A-Za-z0-9_-]` like cell ids, and link paths encode `#`, `?` and
`%` as well as `<` and `>`. The cell id in the heading and alt text is
the sanitized one, so a backtick in an id can't end the code span.
Stream text collapses `\r` frames to the last, so a progress bar's final
line isn't lost to the cut. Markdown cells' attachments are saved like
image outputs and their `attachment:` links point at the files.

These reuse `fenced`, `overwrite_returns` and `replace_attachment_refs`
from `ganymede_nbformat::export`, which are now public; the snapshot's
own fence helper went.
