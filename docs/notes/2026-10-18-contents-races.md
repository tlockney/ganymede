## 2026-10-18, contents races, first checkpoint, trust key (#75)

The review found the `last_modified` conflict check misdocumented and
unlocked, and a few smaller jupyter_server parity and race gaps in
[`ganymede-contents`](../../crates/ganymede-contents/README.md).

- [docs/ipados.md](../ipados.md) said the mtime check on PUT is what
  `jupyter_server` does. It is not: jupyter_server has no such check, and
  JupyterLab compares timestamps itself before saving. The check stays
  as an opt-in extension: only a PUT that sends `last_modified` gets it,
  and a client echoing the value the server reported never gets a 409,
  since that value is the mtime cut to microseconds and the window is
  500 ms. A stale echoed model does get 409 when the file changed since;
  that is the protection the iPad/iCloud case wants, so we kept it.
- The conflict check and the write of one save now run under a per-path
  lock, so two saves of one path cannot both pass the check and then
  interleave.
- The first save of a notebook creates its checkpoint (jupyter_server's
  "One checkpoint should always exist for notebooks"). Before, there was
  nothing to revert to until a client POSTed one.
- `GET ?hash=1` read the file twice; it reads once and hashes the bytes
  it serves.
- `new_untitled` and `copy` picked a free name, then wrote over whatever
  was there. They now claim the name with `create_new` (`create_dir` for
  folders) and move on to the next name if someone else took it.
- Trust: the key file was created, then written; a crash in between left
  an empty key that was accepted forever. The key is now written to a
  0600 temp file and linked into place (no-clobber for a new key, rename
  for `--reset`), and an empty key file is replaced with a new key.

Left out: `rename` still checks that the target is free and then
renames. Closing that needs a no-replace rename (`renameat2` on Linux,
`renamex_np` on macOS), which std does not offer for directories.
