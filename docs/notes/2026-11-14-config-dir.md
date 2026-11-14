## 2026-11-14, ganymede.toml in Ganymede's config dir (#814)

Without `--config`, Ganymede now reads `ganymede.toml` from its own
config dir: `$XDG_CONFIG_HOME/ganymede`, else `~/.config/ganymede`, on
Linux and macOS, and `%APPDATA%\ganymede` on Windows. It was read from
the Jupyter config dir (`~/.jupyter`), which a reader had to know and
which is Jupyter's, not Ganymede's.

macOS uses `~/.config` rather than `~/Library/Application Support`: the
file is for a command-line server, and that is where such tools keep
theirs. The data dir stays in Application Support, as before.

The old file is still read when the new one doesn't exist, with a
warning that names where it should go; when both exist, the new one wins
and the old one is named as ignored. `serve`, `mcp`, `execute`'s host
config and the `crash_reporting` lookup all go through
`serve::default_config`. `ganymede info` prints the config file path.

The Jupyter config dir keeps its Jupyter uses: JupyterLab settings and
workspaces, `labconfig`, `nbconfig` (`--config-dir`). The apps pass their
config as JSON and read no file, so they don't change.
