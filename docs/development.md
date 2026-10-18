# Development

Ganymede is a Cargo workspace; `frontend/` is a separate npm package with
the shared TypeScript for web-view UIs, and `docs/` is this site.

## Build and test

```sh
cargo build --workspace
cargo test --workspace                 # uses an in-process fake kernel
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cd frontend && npm install && npm test
```

The end-to-end tests in `crates/ganymede-cli/tests/` also use whatever real
kernels and Jupyter are installed, and skip what is missing. The
[README](../README.md#testing) lists the variables that make them strict.
CI runs them on Linux and macOS with cdp-kernel and ipykernel, with
`jupyter execute` and `jupyter server` as the reference. Python there is a
test oracle and a kernel, never a dependency of the product. Nothing may
skip in that job; the `rust` job, which has no real kernels, lists what it
skipped in its run summary.

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

- The host knows no language. A grep for `python` in the host crates
  outside tests finds nothing; Python kernels arrive only as installed
  kernelspecs.
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
npm run dev      # live preview
npm run build    # into docs/.vitepress/dist
```

The `pages` workflow builds both on every pull request that touches them,
so a broken page or dead link fails the PR, and deploys them to GitHub
Pages from `main`. The design is cdp-kernel.org's; keep
`site/style.css` and `docs/.vitepress/theme/custom.css` in step with each
other.
