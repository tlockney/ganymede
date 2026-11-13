## 2026-11-13, journal skill: kernel choice and the user's own notebooks (#739)

Two pieces of Thomas's feedback on the journal skill.

**Kernel.** The skill defaulted to `cdp-node` unless the user named a
kernel. It now takes, in order: the user's ask; a folder default from
`.ganymede.toml`, which the server applies when `notebook_open` leaves
out `kernel` (#741); an inference from the project (CLAUDE.md or
AGENTS.md, the kernel of the user's notebooks nearby, `pyproject.toml`
or `package.json`); then `cdp-node`. Thomas's rule: infer only when no
folder setting exists. The skill asks `kernelspecs_list` with
`dir` set to the journal's folder; the plugin's start hook asks only for
the session's own folder, so a `journal/.ganymede.toml` would be missed.

**Journal and notebook.** A user, especially one deep in Jupyter, may do
part of an analysis by hand in their own notebook and part through the
agent. The skill covered a person editing the journal, not the journal
living beside the user's notebooks. It now searches the whole project
(not only `journal/`), works in the user's notebook when that is where
the work lives, matches their kernel and libraries, shares code through
modules rather than copied cells, keeps the journal runnable in plain
Jupyter, and offers to add results to the user's notebook.
