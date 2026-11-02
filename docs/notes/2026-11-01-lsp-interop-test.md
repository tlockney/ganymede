## 2026-11-01, jupyterlab-lsp interop test in CI (#257)

`validation/lsp.mjs` opens a TypeScript file with a type error and a
Python notebook that uses an undefined name in JupyterLab 4.6.5 with
jupyterlab-lsp 5.3.0, and waits for CodeMirror's lint mark on each:
`n` from typescript-language-server, `undefined_name` from pyright. The
notebook goes through the virtual documents path, the `.ts` file
doesn't. `scripts/test-lsp.sh` runs it on `jupyter lab` with jupyter-lsp
2.3.1 (the oracle) and then on `ganymede serve`, and the new `lsp` CI
job runs the script.

Both sides get the same two specs from config, with jupyter-lsp's
`autodetect` off, so the oracle doesn't pick up other servers that
happen to be installed. Ganymede serves the pip-installed JupyterLab and
extension through `--static-dir` and `--labextensions-path`, so both run
the same frontend bundle and nothing is downloaded from PyPI.

The first run passed on both sides, but on Ganymede the page threw
`TypeError: i.getForeignDocuments is not a function` about 13 times per
document, and under jupyter-lsp it threw nothing (#274). jupyterlab-lsp's
`package.json` lists the core plugins it replaces under
`jupyterlab.disabledExtensions` (`@jupyterlab/notebook-extension:language-server`
and three more), and jupyterlab_server adds every enabled extension's
list to the page's `disabledExtensions`. Ganymede read only
`page_config.json`, so the core language-server plugins ran next to
jupyterlab-lsp's. The page config now merges those lists, with
`page_config.json` on top (`true` adds a name, `false` takes one out, and
a disabled extension disables nothing), as jupyterlab_server does. The
test now fails on any page error. Without the fix it fails.

The first CI run of the job failed on Ganymede only: the notebook opened
with "No Kernel", so jupyterlab-lsp never learned its language. jupyter
finds ipykernel's spec in `sys.prefix/share/jupyter`; Ganymede doesn't
know the prefix, and locally a spec in `~/.local` had hidden that. The
script now passes the prefix's `share/jupyter` as `--kernelspec-root`.
