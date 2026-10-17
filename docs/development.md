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
test oracle and a kernel, never a dependency of the product.

## Rules

- The host knows no language. A grep for `python` in the host crates
  outside tests finds nothing; Python kernels arrive only as installed
  kernelspecs.
- The REST and websocket API is a compatibility target, not a design
  space: match `jupyter_server`.
- Each change is a pull request that references a GitHub issue, with CI
  green, and appends what it learned to the
  [development log](notes.md) (`docs/notes.md`).

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
