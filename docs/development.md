# Development

Ganymede is a Cargo workspace, and `docs/` is this site. The TypeScript for
web-view UIs is in [tlockney/ganymede-ui](https://github.com/tlockney/ganymede-ui).

## Build and test

```sh
cargo build --workspace
cargo test --workspace                 # uses an in-process fake kernel
cargo test -p ganymede-contents        # again without the sqlite feature
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```

CI also type-checks the kernel host's Windows code from Linux. To run the
same check, add the target once:

```sh
rustup target add x86_64-pc-windows-gnu
cargo clippy -p ganymede-kernel-host --all-targets --target x86_64-pc-windows-gnu -- -D warnings
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
| `GANYMEDE_E2E_PYTHON=/path/to/python` | The Python for `collab.rs` (needs `jupyter_ydoc`, `pycrdt`, `websockets` 15 or later). When set, it must have them or the test fails. Defaults to `python3`, then `python`, skipping when neither has them. |
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
- `collab.rs`: a `jupyter-collaboration` client
  ([`e2e/collab/ydoc_client.py`](../e2e/collab/ydoc_client.py), on
  `jupyter_ydoc` and `pycrdt`) syncs with a document room, edits the
  notebook while another program changes it on disk, and both sides and
  the saved file must converge.
- `lab.rs`: `ganymede lab install` from PyPI, then `ganymede serve`
  answering `/lab` and `/lab/api/settings` from the installed bundle and
  the default widget extensions.
- `lab_start.rs`: `ganymede lab --no-browser` on a stand-in bundle,
  offline: the port fallback, and the printed URL logging in at `/lab`.
- [`e2e/wasm-smoke/`](../e2e/wasm-smoke/): the engine built for
  `wasm32-wasip1`, running a notebook on the fake kernel under Node's WASI.
  It is its own workspace with its own `Cargo.lock`, which CI builds
  `--locked`. When a change to the engine's dependencies makes that
  lockfile stale, refresh it:
  `cargo update --manifest-path e2e/wasm-smoke/Cargo.toml --workspace`.

CI runs the e2e job on Linux and macOS with Node 24, cdp-kernel 0.10.0,
and ipykernel, plus a portability job for iOS, Android, and wasm, where
a compiler warning fails the build. Its
`workstation` job follows [Install](install.md) on Linux and macOS
with Python hidden from `PATH`
([`scripts/test-workstation.sh`](../scripts/test-workstation.sh)): the PR's
binary through `install.sh`, cdp-kernel from npm, `ganymede lab install`,
`ganymede lab` driven by Playwright (login, trust, widgets, and
`validation/jupyterlab.mjs` running and saving a cell), `ganymede
execute`, nb-cli's remote and gateway modes (`validation/nb-cli.sh`),
`ganymede serve` with the VS Code extension's requests, and the
uninstall, failing if any Python ran. Its `image` job builds `container/Dockerfile` from the
checkout and runs `container/smoke-test.sh` on it. Its `lsp` job
drives JupyterLab with `jupyterlab-lsp` against `ganymede serve` and
against `jupyter lab` with jupyter-lsp, and checks both show the same
diagnostics from typescript-language-server and pyright
([`scripts/test-lsp.sh`](../scripts/test-lsp.sh)). Its `coverage` job
runs the whole suite on Linux under `cargo llvm-cov`, kernels included
but not the `lab install`/`ui install` download tests (the e2e job runs
those), and puts line coverage in the job summary, with a row for each `.rs`
file the pull request changes. It is informational and never fails on a
number. Locally: `cargo llvm-cov --workspace --open`.

## CI

- **Pins.** The cdp-kernel, ipykernel, nbclient, and `jupyter_server`
  versions the end-to-end jobs install are in one place, the `env:` block
  at the top of `.github/workflows/ci.yml`. Bump them together in one
  pull request, with the `CDP_KERNEL_VERSION` default in
  `container/Dockerfile` and the
  [tested versions](architecture.md#tested-versions) table; for
  cdp-kernel, `scripts/release.sh` does this in the release commit
  ([Releasing](releasing.md)). A weekly
  scheduled run installs the latest releases instead, so drift shows up
  there first. `NB_CLI_VERSION` (nb-cli, for the workstation job) is
  pinned there too and also goes to the latest release on the scheduled
  run.
- **Base images.** `container/Dockerfile` pins each base image by tag and
  multi-arch digest, and Dependabot's weekly `docker` group updates
  them. Keep the `rust` tag the same as the `linux-release` job's
  container in `ci.yml`.
- **Rust versions.** CI builds with the latest stable. `rust-version` in
  the workspace `Cargo.toml` is the oldest supported Rust, and the `msrv`
  job builds with exactly that; raise it on purpose.
- **Dependencies.** The `audit` job runs `cargo deny` (`deny.toml`):
  RustSec advisories fail it, duplicate versions show as warnings. A pull
  request checks advisories only when it changes `Cargo.lock`, a
  `Cargo.toml` or `deny.toml` (the `changes` job's `deps` output), so a
  new advisory doesn't fail unrelated pull requests; pushes to `main` and
  the weekly run check them every time. Licenses are checked by `cargo about` against `about.toml`.
  Dependabot (`.github/dependabot.yml`) opens weekly grouped bumps for
  Cargo, npm, and the actions; its pull requests are exempt from the
  linked-issue check.
- **Docs-only changes.** A pull request, or a push to `main`, that
  changes only `docs/`, `site/` (other than `site/install.sh`), or
  Markdown builds nothing: the `changes` job (`scripts/ci-changes.sh`)
  says so, and every job reports success without running. The `Pages`
  workflow still builds the site. Anything else, the weekly schedule and
  manual runs run the whole CI. A failed weekly run opens a "Weekly CI run
  failed" issue, or comments on the open one. Add a path to the script's list only if
  no job can be affected by it.
- **The XCFramework.** `ffi (macos-latest)` builds it only when the change
  can affect ganymede-ffi (the script's `ffi` output: anything but
  `ganymede-cli`, `ganymede-engine`, the test and tooling directories,
  and other scripts and workflows), and both `ffi` cells build without
  LTO except on the weekly schedule. The release profile build is also
  `release.yml`'s, on every push to `main` and every tag.
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
- Status lives in two pages: [Known gaps](known-gaps.md) (what doesn't
  work) and the [Roadmap](roadmap.md) (what is done and planned). Other
  pages and crate READMEs describe what is there and link to those two
  instead of saying "not yet"; status repeated elsewhere went stale in
  each docs review.

## Releases

`scripts/release.sh <level>` bumps the workspace version, writes
`CHANGELOG.md`, commits on `release/vX.Y.Z`, and opens an issue and a PR
for it. Squash-merging the PR publishes `vX.Y.Z` from the release
commit's push to `main` and tags it. It is a dry run without
`--execute`. [Releasing](releasing.md) has the whole path, and what to
do when a release fails partway.

## This site

The landing page is plain HTML and CSS in `site/`. These docs are
Markdown in `docs/`, built by VitePress:

```sh
cd docs && npm install
npm run fetch-ui # ganymede-ui's docs, from its main branch
npm run fetch-cell-api # the cell API reference, from cdp-kernel and ganymede-device
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

The pages under `/docs/cell-api/` are the reference pages that
[cdp-kernel](https://github.com/tlockney/cdp-kernel)
(`docs/jupyter-reference.md`, `docs/widgets-reference.md`) and
[ganymede-device](https://github.com/tlockney/ganymede-device)
(`docs/reference.md`) generate from the doc comments on their types,
copied into `docs/cell-api/` (not committed) by `npm run fetch-cell-api`;
set `CDP_KERNEL_DIR` or `GANYMEDE_DEVICE_DIR` to copy from a local
checkout. Edit the doc comments there. The script points links to
cdp-kernel's guides at cdp-kernel.org, and other links that leave the
pages at GitHub. The site picks up changes to them on its next build.

The `Pages` workflow builds both on every pull request that touches them,
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
and removes the notes ([Releasing](releasing.md)). Don't say it any other
way, such as a word in parentheses after the feature: the release
wouldn't find it. `npm run check-unreleased` in `docs/`, which the `Pages`
workflow runs, fails on one. A change to a feature that is already
released needs no note.
`site/install.sh` goes live on merge too, and must work with the latest
release. Diagrams are SVG files in `docs/images/`, with their dark colours in a
`@media (prefers-color-scheme: dark)` block, so they follow the OS on
GitHub. The docs build writes a light and a dark copy of each into
`docs/images/themed/` (not committed), and the site shows the one that
matches its theme toggle ([`themed.ts`](.vitepress/themed.ts)); a new
diagram needs nothing more than that block. Give each a legend for any
colour or line style that carries meaning.
The design is cdp-kernel.org's; keep
`site/style.css` and `docs/.vitepress/theme/custom.css` in step with each
other.

### App screenshots

The landing page's Apps section shows the Mac, iPad and iPhone apps from
`docs/images/apps/`: `{mac,ipad,iphone}-{light,dark}.png`, the e2e
fixture `Orbits.ipynb` with its outputs. Re-shoot them on a Mac when the
apps' look changes, and after a ganymede-ui release, with
[`docs/scripts/app-screenshots.sh`](scripts/app-screenshots.sh) and a
ganymede-ui checkout:

```sh
docs/scripts/app-screenshots.sh ~/src/ganymede-ui docs/images/apps
```

It needs Xcode, the iOS simulators, and `ganymede` with cdp-kernel (it
executes the notebook first). It builds both apps from the checkout
against the ganymede revision ganymede-ui's CI pins, boots the largest
iPad Pro and newest iPhone Pro simulators with the status bar at 9:41,
and captures each device in light and dark. It works in
`docs/scripts/work/` (git-ignored); keep that path free of dot-folders,
which the server treats as hidden. Some steps still need a person:

- **iOS.** Rotate the iPad to landscape, and tap Orbits on each device;
  the script waits for Return. The app has no launch argument or URL to
  open a notebook yet.
- **Mac.** The capture needs Screen Recording in the shell that runs
  it, and the dark shot needs the Mac in dark mode (the light one
  overrides the app's appearance). When the shell that can `open` apps
  lacks Screen Recording, split the shot:
  `--mac-launch light|dark` in one shell, `--mac-capture light|dark` in
  one that has it.

The copies drop the fixture's deliberate error cell (`--keep-error-cell`
keeps it). The Mac app's preferences go to `work/home`, so other builds
on the Mac keep theirs. Don't shoot Settings or other windows that are
being reworked; and check every picture before committing it.
