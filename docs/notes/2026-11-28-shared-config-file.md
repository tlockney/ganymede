## 2026-11-28, one config file for the app and the server (#995)

`ganymede.toml` can now be the Mac app's settings file as well as
`ganymede serve`'s, one file both read: `ganymede_config_read` gives its keys as the JSON
object `ganymede_server_start` takes as `config_json`, and
`ganymede_config_set` applies a JSON object to the file. The app will add
the keys only its launch needs (`root_dir`, `static_dir`, `port`,
`cookie_secret_file`) and passes the result, so a setting made in the app
is one the server reads on its next start, and a comment or key a person
wrote in the file survives the app's edits. Swift never writes TOML.

Decisions worth the record:

- **`ganymede.toml`, not a second file.** The app could keep its settings
  in its own file, or in `UserDefaults`, and pass them to
  `ganymede_server_start` alone. Then the background `ganymede serve`
  would read neither, and a setting made in the app — MCP on, a tailnet
  login allowed — would hold only while the app's server ran. One file
  means one answer to "what are this machine's Ganymede settings": the
  app's launch keys and the server's are the same document, and the
  keys the app adds (`root_dir`, `static_dir`, `port`,
  `cookie_secret_file`) are ones `ganymede serve` reads, not a private
  side channel.
- **`toml_edit`, not serde.** `set` parses the file into a `DocumentMut`
  and merges the patch into it, so comments, key order, whitespace and
  keys the patch doesn't name are untouched. Serializing a `Config` back
  out would keep only the keys `Config` knows, in its order, with its
  formatting: a file the app wrote would lose everything a person put in
  it. `toml_edit` is the same maintainers and parser as `toml`, which the
  server already had, and it is added to the workspace at `0.25` beside
  it.
- **The whole module is in `ganymede-server`, not `ganymede-ffi`.** The
  file's path comes from `serve::ganymede_config_dir` and
  `DEFAULT_CONFIG_FILE`, and the check that an edit is valid is
  `Config::from_toml`, both in that crate; the C ABI functions are
  wrappers over `config_file::read` and `config_file::set`. So the CLI,
  the tests and an embedder all go through one implementation of what
  the file means.
- **A patch merges; `null` deletes.** A value sets a key, `null` removes
  it, and an object merges into that table recursively rather than
  replacing it, since the app sets one setting at a time and a whole
  `[tailscale]` replaced to change `allow` would drop `whois`. A table
  the patch creates is a standard table with a blank line before it in a
  document with keys already, as a person writes them, and an inline
  table inside an inline table, so a `static_headers = { … }` file stays
  as it is.
- **An invalid edit writes nothing.** The edited text is checked with
  `Config::from_toml` before anything is written, and its error names the
  file, so a misspelt key is refused with the same "unknown field" a
  server would report rather than written and failing at the next start.
  The check is on the whole file, not the patch: a file that was already
  invalid stays invalid and every edit of it is refused, which is the
  honest answer — the app can't make it valid by adding one key.
- **The file is replaced atomically, 0600, in a 0700 dir.** A temporary
  file in the same directory (`.ganymede.toml.<pid>.<n>.tmp`, `O_EXCL`,
  0600) is written, synced and renamed over the file, so a reader sees
  the old file or the new one; the config can hold a `token`, so the file
  and a dir the app creates are private. A symlinked file is written
  through the link — `symlink_metadata` first, then `canonicalize` — so a
  dotfile manager's link keeps pointing at the file it names instead of
  being replaced by a regular file.
- **`read` returns the file's keys, not `Config`'s.** It doesn't
  validate: a key from a newer Ganymede, or one only the app's launch
  uses, comes back, and a key the current `Config` doesn't know doesn't
  make the read fail. That is what makes `read` → add the launch keys →
  `ganymede_server_start` work for a file written by either side.
- **A legacy file is read but not edited.** With `dir` NULL,
  `serve::find_default_config` may find `ganymede.toml` in the Jupyter
  config dir, which the server still reads; `read` and `set` decide with
  that same function, so a dangling link in the config dir counts as no
  file in both, and `read` reads that file, so the app
  shows what the server is serving, but `set` refuses to create a new
  file beside it — the server would then read the new file and silently
  ignore the settings in the old one — and says to move it.
- **Numbers keep their TOML meaning.** A JSON number that fits an `i64`
  is an integer, else a float; a `u64` above `i64::MAX` is an error
  rather than a float, since TOML integers are signed and writing it as a
  float would change it. A datetime, which JSON has no type for, reads as
  its TOML text.
- **One lock, in process.** `set` calls in one process take turns on a
  mutex, since the C functions take any thread and two threads would
  otherwise both read the old file and the second rename drop the first's
  edit. Across processes nothing is locked: each call re-reads the file,
  so two processes lose only an edit made during the other's read and
  write. A lock file in the config dir would be a second thing to keep
  correct, and only the app writes settings.

The header's two functions are new, so the docs for them carry the
release note: `docs/embedding.md` lists both, and `docs/reference.md`'s
config-file section says a program can edit the file through them. The
Mac app doesn't yet, so what it will keep there is described as the plan,
with a `**Unreleased in ganymede-ui.**` comment to update.

The follow-up is in ganymede-ui, ganymede-ui#902: the app moves the settings it keeps in
`UserDefaults` — the MCP switches, `[tailscale] allow`, the `[host]`
defaults — into this file, through these two functions, so they outlive
the app's server and are one set with the server's.

Tests:

- `ganymede-server/src/config_file.rs`, unit: a set changes only its key's
  line and keeps a comment beside it; a new key goes at the end; a new
  table, and a subtable of one, are standard tables, with a blank line
  before them, and no blank line in an empty file; an existing table and
  an existing inline table are merged into, keeping the comment above the
  table; `null` removes a key from the document and from a table, and
  changes nothing for a key that isn't there; a refused edit — unknown
  key, unknown key in a table, wrong type, `null` in an array, not an
  object, `u64` too large — leaves the file byte-for-byte as it was and no
  temporary file behind; the read returns tables as objects, arrays,
  datetimes as text and an unknown key; a file that is not TOML is an
  error naming it; a new dir and file are 0700 and 0600, an existing 0644
  file becomes 0600; a symlink stays a symlink and the target changes; a
  dangling symlink is an error; with a Jupyter config dir, the legacy file
  is read, a broken one is named in the error, a set is refused and
  creates nothing, a set with both files writes the new one, and a
  dangling link reads the legacy file as the server does; a patch that
  changes nothing creates no file; a failed rename leaves no temporary
  file; and 64 threads setting different keys all land.
- `ganymede-ffi/tests/config_default_dir.rs`, its own binary since it
  sets `GANYMEDE_CONFIG_DIR` and `JUPYTER_CONFIG_DIR`: with a NULL dir,
  read gives the legacy file, set refuses and creates nothing, and after
  the file is moved set writes it in the config dir.
- `ganymede-ffi/tests/config.rs`, from C's side: set then read gives the
  keys back and the file keeps its comment; a refused set returns nonzero
  with a message naming the file and changes nothing; a NULL `json` and a
  patch that isn't an object are errors; a missing file reads as `{}`, with
  a NULL `error_out` allowed; a file that is not TOML returns NULL with a
  message.
- `smoke.c`, which CI builds and runs: a temp dir reads as `{}`, a set
  writes it, and the read gives the key back.

Removing the `Config::from_toml` check in `apply_to` fails
`a_refused_edit_leaves_the_file_as_it_was` (it writes the bad key instead)
and `a_refused_edit_says_so_without_a_path`; it is restored after checking,
and the workspace's tests and clippy are run with it restored.
