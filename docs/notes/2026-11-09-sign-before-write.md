## 2026-11-09, sign before writing a notebook (#617)

`mcp_journals_into_a_notebook` failed once on macOS at "journal signed".
The room's saves are all trusted (no outputs came from an unsigned file),
so the cause was not `mark_trusted`: `FsContents::save` wrote the file and
then stored its signature. The test polls the file, and a read between the
two found the new contents with no signature yet. Any reader could hit the
same window, such as a frontend opening the file just after a room save,
which would show it untrusted.

jupyter_server's `FileContentsManager.save` calls `check_and_sign` before
`_save_notebook`, so the save now signs first. A failed write leaves a
signature for contents that never reached disk, as in jupyter_server; it
does no harm, since nothing on disk matches it.

`the_signature_is_stored_before_the_file_is_written` (in
`crates/ganymede-contents/tests/trust.rs`) uses a signature store that
records whether the file existed when the signature was stored.
