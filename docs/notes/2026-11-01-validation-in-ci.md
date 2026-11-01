## 2026-11-01, JupyterLab and nb-cli validation run in CI (#256)

`validation/jupyterlab.mjs` and `validation/nb-cli.sh` only ran by hand.
The workstation job now runs both, with Python hidden:

- `jupyterlab.mjs` on the `ganymede lab` server the job already starts:
  open a notebook, run a TypeScript cell on cdp-node, save, and check the
  saved output. The script's defaults are Python, so the job sets `CODE`
  and `EXPECT`.
- `nb-cli.sh` on cdp-node, when `NB` names an nb-cli binary. CI builds
  nb-cli `NB_CLI_VERSION` (0.0.10) with `cargo install --locked` and
  caches the binary by version. It is a test tool, not a dependency of
  the product. With Python hidden, this also shows nb-cli's remote and
  gateway modes need no Python once `--server`/`--token` are given.

python3 isn't covered here because the workstation job has no Python;
nb-cli on ipykernel stays a by-hand run.
