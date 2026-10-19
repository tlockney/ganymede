# Development

Ganymede is a Cargo workspace, and `docs/` is this site. The TypeScript for
web-view UIs is in [tlockney/ganymede-ui](https://github.com/tlockney/ganymede-ui).

## Build and test

```sh
cargo build --workspace
cargo test --workspace                 # uses an in-process fake kernel
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```

The end-to-end tests in `crates/ganymede-cli/tests/` also use whatever real
kernels and Jupyter are installed, and skip what is missing (variables
[below](#end-to-end-tests)).
CI runs them on Linux and macOS with cdp-kernel and ipykernel, with
`jupyter execute` and `jupyter server` as the reference. Python there is a
test oracle and a kernel, never a dependency of the product. Nothing may
skip in that job; the `rust` job, which has no real kernels, lists what it
skipped in its run summary.

### End-to-end tests

These variables make the end-to-end tests strict or add to them:

| Variable | Effect |
|---|---|
| `GANYMEDE_E2E_KERNELS=cdp-node,python3` | These kernels must exist; their tests fail instead of skipping. |
| `GANYMEDE_E2E_JUPYTER=/path/to/jupyter` | The reference `jupyter` (needs `nbclient` and `jupyter_server`). Defaults to `PATH`. |
| `GANYMEDE_E2E_REQUIRE_JUPYTER=1` | Fail instead of skipping when it is missing. |
| `E2E_DUMP=1` | Print both server transcripts. |
| `GANYMEDE_E2E_LAB=1` | Run `lab.rs`, which downloads JupyterLab and the widget extensions from PyPI (about 18 MB). |

- `kernels.rs`: the same scenarios on every kernel, through the engine and
  the CLI: kernel_info passthrough, results, streams, errors, stdin,
  interrupt, restart, comms, completion, inspection, standalone kernels.
- `parity.rs`: each notebook in [`e2e/notebooks/`](../e2e/notebooks/) is
  executed by `jupyter execute` and by `ganymede execute`, and the files
  are compared after normalizing timestamps and ids. Mismatches are kept
  under `$TMPDIR/ganymede-parity-*`. Also checks that `ganymede trust` and
  `jupyter trust` share one signature store.
- `server.rs`: one REST and websocket scenario against `jupyter server`
  and `ganymede serve`, compared as transcripts, including which cells come
  back `trusted` after each kind of save and after `POST .../trust`.
- `lab.rs`: `ganymede lab install` from PyPI, then `ganymede serve`
  answering `/lab` and `/lab/api/settings` from the installed bundle and
  the default widget extensions.
- `lab_start.rs`: `ganymede lab --no-browser` on a stand-in bundle,
  offline: the port fallback, and the printed URL logging in at `/lab`.
- [`e2e/wasm-smoke/`](../e2e/wasm-smoke/): the engine built for
  `wasm32-wasip1`, running a notebook on the fake kernel under Node's WASI.

CI runs the e2e job on Linux and macOS with Node 24, cdp-kernel 0.6.0,
and ipykernel, plus a portability job for iOS, Android, and wasm. Its
`workstation` job follows [Install](install.md) on Linux and macOS
with Python hidden from `PATH`
([`scripts/test-workstation.sh`](../scripts/test-workstation.sh)): the PR's
binary through `install.sh`, cdp-kernel from npm, `ganymede lab install`,
`ganymede lab` driven by Playwright, `ganymede execute`, `ganymede serve`
with the VS Code extension's requests, and the uninstall, failing if any
Python ran.

## CI

- **Pins.** The cdp-kernel, ipykernel, nbclient, and `jupyter_server`
  versions the end-to-end jobs install are in one place, the `env:` block
  at the top of `.github/workflows/ci.yml`. Bump them together in one
  pull request. A weekly scheduled run installs the latest releases
  instead, so drift shows up there first.
- **Rust versions.** CI builds with the latest stable. `rust-version` in
  the workspace `Cargo.toml` is the oldest supported Rust, and the `msrv`
  job builds with exactly that; raise it on purpose.
- **Dependencies.** The `audit` job runs `cargo deny` (`deny.toml`):
  RustSec advisories fail it, duplicate versions show as warnings. It also
  runs weekly. Licenses are checked by `cargo about` against `about.toml`.
  Dependabot (`.github/dependabot.yml`) opens weekly grouped bumps for
  Cargo, npm, and the actions; its pull requests are exempt from the
  linked-issue check.
- **Actions** are pinned by full commit SHA, with the version in a
  comment. Let Dependabot move them; don't hand-edit a tag back in.
- **Permissions.** Every workflow starts from `contents: read` (or
  nothing), and a job that needs more asks for it itself.

## Rules

- The host knows no language. No code depends on a kernel's language or
  branches on it, and Python kernels arrive only as installed
  kernelspecs. Outside tests, `python` appears in the host crates only
  where a file format is defined by Python's output (nbformat's float
  layout, the trust signature's scalar format), in PyPI's names
  (`ganymede lab install` fetches JupyterLab's wheels from there), and
  in comments that say what Python does.
- The REST and websocket API is a compatibility target, not a design
  space: match `jupyter_server`.
- Each change is a pull request that references a GitHub issue, with CI
  green, and adds what it learned to the
  [development log](notes.md) as its own file,
  `docs/notes/YYYY-MM-DD-<topic>.md`. Don't append to `docs/notes.md`:
  one shared file makes every pair of open PRs conflict.

## Releases

`scripts/release.sh <level>` bumps the workspace version, writes
`CHANGELOG.md`, commits on `release/vX.Y.Z`, and opens an issue and a PR
for it. Merging the PR tags `vX.Y.Z`, which runs the release workflow. It
is a dry run without `--execute`. [Releasing](releasing.md) has the
whole path, and what to do when a release fails partway.

## This site

The landing page is plain HTML and CSS in `site/`. These docs are
Markdown in `docs/`, built by VitePress:

```sh
cd docs && npm install
npm run fetch-ui # ganymede-ui's docs, from its main branch
npm run dev      # live preview
npm run build    # into docs/.vitepress/dist
```

The pages under `/docs/ganymede-ui/` are
[tlockney/ganymede-ui](https://github.com/tlockney/ganymede-ui)'s
`docs/*.md`, copied into `docs/ganymede-ui/` (not committed) by
`npm run fetch-ui`; set `GANYMEDE_UI_DIR` to copy from a local checkout
instead. Edit them in ganymede-ui. Without the copy the site builds
without them. ganymede-ui's `docs` workflow sends a `repository_dispatch`
(`ganymede-ui-docs`) when they change on its `main`, which rebuilds and
deploys this site.

The `pages` workflow builds both on every pull request that touches them,
so a broken page or dead link fails the PR. It deploys them to GitHub
Pages from `main` on every merge that touches them, and again after each
release. `install.sh` installs the latest release, so docs for a feature
that isn't released yet carry this note, one per section the feature
adds, next to it in the docs page, README, or landing page:

```md
> [!NOTE]
> **Unreleased.** Available only from a build of `main` (HEAD), not in
> the latest release.
```

Keep the words `**Unreleased.**` as they are: the release looks for them
and removes the notes ([Releasing](releasing.md)). A change to a feature
that is already released needs no note.
`site/install.sh` goes live on merge too, and must work with the latest
release. The design is cdp-kernel.org's; keep
`site/style.css` and `docs/.vitepress/theme/custom.css` in step with each
other.
