# Validation against real clients

Scripts that drive real Jupyter clients against a running server. They
need a kernel and a browser. CI's `workstation` job runs `login.mjs`,
`trust.mjs`, and `widgets.mjs` on cdp-kernel through
[`../scripts/test-workstation.sh`](../scripts/test-workstation.sh); the
rest are run by hand. Results and what broke are recorded in
[`../docs/notes.md`](../docs/notes.md).

```
npm install
# JupyterLab 4 in headless Chromium. Serve its bundle first, e.g.
#   ganymede-server --static-dir <prefix>/share/jupyter/lab/static \
#     --labextensions-path <prefix>/share/jupyter/labextensions --token tok
node jupyterlab.mjs http://127.0.0.1:8888 tok <kernel> out/lab.json
# The URL `ganymede lab` printed logs in: lands on /lab with the cookie.
node login.mjs 'http://127.0.0.1:8888/lab?token=tok' cdp-node out/login.png
# A cdp-kernel widget (anywidget) rendering in JupyterLab, clicked, and
# read back from the kernel. Needs `ganymede lab install`'s extensions.
# RELOAD=1 also saves, reopens, and checks the widget renders again.
RELOAD=1 node widgets.mjs http://127.0.0.1:8888 tok cdp-node out/widgets.json
# Notebook trust: HTML and JavaScript outputs run again when a saved
# notebook is reopened, and not in a copy no client vouched for.
WS_TOKEN=1 node trust.mjs http://127.0.0.1:8888 tok python3 out/trust.json
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
