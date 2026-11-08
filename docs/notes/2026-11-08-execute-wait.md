## 2026-11-08, execute endpoint waits for its batch (#566)

The apps' Shortcuts actions (ganymede-ui#302) call the in-process server
over REST. `POST /api/kernels/{id}/execute` answered once the batch was
queued, and a REST client had no way to learn when it finished: the
outputs are in the room at once, but in the file only after the autosave
a second later, and the room's state reaches clients only over the
websocket. An optional `wait` (seconds) makes the reply wait for the
batch and carry the cells as the room has them.

The cells are read with the room's own `Get` command after the batch's
`finished` signal. The worker sends each cell's `Done` to the room before
it signals, and the room handles commands in order, so the cells read
carry their final outputs. Without `wait` the answer stays `200 null`,
as `jupyter-server-documents` answers, so nb-cli and JupyterLab see no
change.
