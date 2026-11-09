## 2026-11-09, cdp-kernel's Node floor is 22.18, not 24 (#632)

Item 7 of the project read of 2026-11-09 (#629).

`site/install.sh` skipped cdp-kernel below Node 24 and said "cdp-kernel
needs Node 24 or later", and the README, the install page, the landing
page and the install diagram said the same. cdp-kernel's floor is Node
22.18: its `engines`, its README and its CI, which tests 22, 24 and 26.
So someone on Node 22 LTS got Ganymede with no kernel and a message that
was wrong. The 24 came from what Ganymede's own CI runs, which is a
choice of what to test with; CI stays on 24.

- The installer compares major and minor now. The old check took the
  major alone from `node -p`. The new one reads `process.versions.node` and
  splits it in the shell, and treats anything that isn't a number as 0,
  so an odd `node` gets the upgrade hint and not an `npm install`.
- `scripts/test-install.sh` runs the installer with a stub `node` at
  22.18.0, 22.23.4, 23.0.0 and 26.1.0 (kernel installed) and at 22.17.1,
  20.19.5 and 18.20.8 (hint, no `npm`, exit 0). The second group fails
  against the old installer.
- The installer goes live when this merges and installs the latest
  release as before, so nothing here waits for a release and nothing
  needs an "Unreleased" note.

Also in this PR: `docs/architecture.md` no longer describes ganymede-ui's
`packages/client`, which tlockney/ganymede-ui#452 deletes.
