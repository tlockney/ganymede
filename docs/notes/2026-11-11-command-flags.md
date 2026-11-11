## 2026-11-11, Command flags generated from the CLI (#689)

reference.md listed every command but only the global and server flags;
50 of the commands' own flags (`execute --inplace`, `run --json`, `mcp
--read-only` and the rest) were only in `--help` or scattered through
the guides. A "Command flags" section now holds a table per command,
generated from clap's definition by a test in ganymede-cli
(src/reference.rs), so `--help` and the page can't drift: the test fails
when they differ, and `UPDATE_REFERENCE=1` rewrites the section between
its markers. The help text is the flag's doc comment, with the default
appended when clap has one. `serve` is left out and `lab` and `ui` skip
the flags they share with it, since the "Server flags" table already has
those with their config keys. Global flags are the root's own table;
clap hasn't propagated them when the test reads the definition, so a
subcommand lists only what it declares (`mcp --server`, not
`mcp call --server`).

The section is generated rather than the whole page because the rest of
reference.md (the config file, the host API, exit status) is prose that
reads better written by hand. scripts/ci-changes.sh now counts
docs/reference.md as code, so a docs PR that edits the section by hand
runs the test.

Review fix: the first version left seven flags out. It dropped `serve`'s
flags from every command, not only from `lab` and `ui`, which take them
all, so `grant --root-dir`, the `mcp` flags and `trust --notary-dir`
went missing. It also dropped every flag named `--version`, which took
`lab install --version` and `ui install --version` with clap's own.
Now only `lab` and `ui` skip the server flags, and only clap's help and
version actions are skipped.

The "Flags for every command" table is generated now too, from the root
command, between its own markers, so it uses `--help`'s wording and value
names. Flags that can repeat say so, since `--help` doesn't. The
"Server flags" table stays hand-written, because it has the config keys.
