## 2026-11-03, nb-cli on document rooms (#278)

nb-cli was validated on 2026-10-31, before Ganymede served document
rooms, so it used the contents API. On `main` (debd6e8) `nb connect`
reports `Mode: collaborative`: nb-cli edits cells through the room and
runs them with jupyter-server-documents' execute endpoint.
`validation/nb-cli.sh` passes on cdp-kernel and ipykernel; the
workstation CI job runs it on cdp-kernel since #283, which adapted it to
this path.

`docs/agents.md` and `docs/clients.md` now describe the room path, its
live view in JupyterLab (with the collaboration extensions) and
ganymede-ui, and nb-cli 0.0.10's two quirks on it: it resolves cell
indexes from the file while editing the room, and it drops
`execute_result` outputs read from a room (#296). The agents diagram
names both paths, since the latest release still has only the contents
API.
