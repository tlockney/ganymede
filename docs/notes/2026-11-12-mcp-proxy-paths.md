## 2026-11-12, `ganymede mcp` paths start in the agent's folder (#729)

The Mac app is to serve `/mcp` and write a server info file (ganymede-ui
#304), so agents configured with `ganymede mcp` reach the app when it
runs and a server of their own when it doesn't. The app's server is
rooted at `/`, so a forwarded `a.ipynb` meant `/a.ipynb`, not the file in
the agent's folder.

`ganymede mcp` now sends the agent's folder, relative to the server's
root, as `_meta["ganymede/dir"]` on `initialize`. The server keeps it per
connection: paths without a leading `/` start there, and paths in
answers are shown from there (below it) or from the root with a leading
`/` (elsewhere, and the folder itself), so what an answer shows reads
back to the same file.

- `_meta` rather than a header: a folder name may be any Unicode, and a
  header value would need encoding on both sides.
- The proxy can't rewrite paths itself: answers carry root-relative paths
  in tool text, which an agent passes back.
- `..` is still refused, as the contents layer does; a leading `/`
  reaches a file outside the folder.
- A change for anyone running `ganymede mcp` below a `ganymede serve`
  root: `sub/a.ipynb` from `sub/` is now `a.ipynb` (or `/sub/a.ipynb`).
