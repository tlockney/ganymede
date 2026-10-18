## 2026-10-18, MIT license file and third-party notices (#64)

README and `Cargo.toml` said MIT, but no `LICENSE` file shipped, so GitHub
detected none and the release archives carried no copyright notice.

- Audit before adding it: of the 329 crates in `Cargo.lock`, every license
  is permissive or offers a permissive choice. The ones that are not plain
  MIT/Apache: `option-ext` is MPL-2.0 (file-level copyleft; using it
  unmodified as a dependency puts nothing on Ganymede), `r-efi` offers
  LGPL only as one of three choices, `webpki-roots` is CDLA-Permissive-2.0
  (the Mozilla root list), `clipboard-win` and `ryu` allow BSL-1.0,
  `aws-lc-sys` and `ring` combine ISC, Apache-2.0 and BSD-3-Clause. SQLite (bundled
  by rusqlite) is public domain. The npm packages in `docs/`, `frontend/`
  and `validation/` are dev tools, never shipped, and are MIT/BSD/ISC/
  Apache anyway. JupyterLab (BSD-3-Clause) is downloaded by
  `ganymede lab install` at run time, not redistributed.
- `cargo about` writes `THIRD-PARTY-NOTICES` from `about.toml` (accepted
  licenses) and `about.hbs` (plain-text template, triple-stash so names
  are not HTML-escaped). `private = { ignore = true }` drops the
  workspace's own crates; build and dev dependencies are ignored since they
  are not linked. The file is generated per target in `release.yml`
  (Windows and macOS trees differ) and is not committed, so dependency
  bumps never need a regenerated file.
- CI runs the same generation with `--fail` and output to `/dev/null`: a new
  dependency under a license missing from `about.toml` fails the build.
- `cargo install cargo-about` builds no binary without `--features cli`;
  CI uses `taiki-e/install-action` instead.
