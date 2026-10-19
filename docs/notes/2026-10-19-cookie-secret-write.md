## 2026-10-19, cookie secret written without replacing or following links (#169)

`load_cookie_secret` opened `jupyter_cookie_secret` with `create` and `truncate`, so it wrote through a symlink planted at that path, and two servers starting together each wrote their own key, the second invalidating cookies signed with the first. It now writes the key the way the notebook-signing key is written (`contents/src/trust.rs`): a mode-0600 temp file in the same directory, synced, then `persist_noclobber` into place. A server that loses the race reads and uses the winner's key. An empty file is replaced with `persist`, which renames over the path and so replaces a symlink rather than its target.

Lesson: every secret file in the tree should go through temp file plus rename. `OpenOptions::create(true).truncate(true)` follows links and races, and `create_new` alone does not stop a second writer from losing its key.
