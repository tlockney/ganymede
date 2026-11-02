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
  `nb create` does run `python3 -c 'import jupyter_core.paths ...'`
  (then `python`) to find kernelspecs, and falls back to the platform
  dirs when that fails. The first CI run caught it through the
  workstation script's shims and failed "No Python". That probe is
  nb-cli's, so `nb` runs through a wrapper whose `PATH` leaves the shims
  out, where python isn't found, as on a machine without it. The
  `ganymede serve` that nb-cli.sh starts keeps the shims.
- nb-cli 0.0.10 uses jupyter-server-documents' API when the server
  answers `POST /api/fileid/index` (#284 added it), so on `main` it
  edits through a document room and runs cells with
  `POST /api/kernels/{id}/execute`, not the contents API. #284 changed
  nb-cli.sh's header but not its checks, and no CI ran the script, so on
  `main` it failed at once: it still expected `nb connect` to report
  direct mode. It now expects `Mode: collaborative`. Its checks of the
  file on disk poll for up to 5 s, since the room saves shortly after a
  change rather than on each request.
- Room mode exposed two nb-cli gaps. It counts cells and resolves
  indexes such as `--cell-index -1` from `GET /api/contents`, which reads
  the file, while its edits go through the room. Three `nb cell add`
  calls inside the room's 1 s save delay all inserted at index 1, and the
  following delete refused ("Cannot delete all cells"). The script now
  waits for each edit to reach the file. And nb-cli's yrs 0.27 reads a
  Yjs integer as a float, so it drops every `execute_result` it reads
  from the room (#296). `READ_CODE` now prints, and execution counts
  are checked on disk. nb-cli's empty-body probe of the execute
  endpoint gets the 400 it expects, so the script allows that one.

python3 isn't covered here because the workstation job has no Python;
nb-cli on ipykernel stays a by-hand run.
