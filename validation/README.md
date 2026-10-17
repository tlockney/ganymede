# Validation against real clients

Scripts that drive real Jupyter clients against a running server. They
need a kernel and a browser, so they are not part of CI. Results and what
broke are recorded in [`../docs/notes.md`](../docs/notes.md).

```
npm install
# JupyterLab 4 in headless Chromium. Serve its bundle first, e.g.
#   ganymede-server --static-dir <prefix>/share/jupyter/lab/static \
#     --labextensions-path <prefix>/share/jupyter/labextensions --token tok
node jupyterlab.mjs http://127.0.0.1:8888 tok <kernel> out/lab.json
# The VS Code Jupyter extension's remote-server flow, replayed with the
# same @jupyterlab/services settings, Request, and WebSocket overrides.
CODE='print(6*7)' STDIN_CODE='print(input())' SLEEP_CODE='import time; time.sleep(30)' \
  node vscode.cjs 'http://127.0.0.1:8888/?token=tok' <kernel>
```

`jupyterlab.mjs` writes every request the page made to the output JSON,
with a screenshot next to it. It opens the page with `?token=` and relies
on the login cookie for websockets, as a browser does. The `CODE`, `EXPECT`,
`STDIN_CODE`, and `SLEEP_CODE` defaults are Python; set them for other
kernels.
