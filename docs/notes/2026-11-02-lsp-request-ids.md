## 2026-11-02, Language server requests per client (#269)

- One language server process serves every tab, and each `jupyterlab-lsp`
  tab numbers its requests from its own counter, so two tabs send the
  same ids. The relay (`ganymede-lsp/src/router.rs`) now gives each
  client request a relay id on the way to the server and sends the
  response back to the client that asked, with its own id.
  `$/cancelRequest` is rewritten to the relay id. `jupyter-lsp` still
  broadcasts everything.
- A request from the server (`workspace/configuration`,
  `client/registerCapability`) goes to the first connected client, and
  only that client's answer reaches the server. If that client leaves
  first, the request is put to the next one.
- The workspace builds `serde_json` with `preserve_order`, so a
  re-serialized message keeps its key order and `id` isn't first. The
  relay test's fake server is a Perl one-liner (both CI runners have
  Perl), since `cat` can't answer a request.
