## 2026-11-28, opt-in update check (#956)

`ganymede serve`, `lab` and `ui` can now check GitHub for a newer release
and report it at `GET /api/update`: `--check-for-updates`, or
`check_for_updates = true` in the config file. It is off by default, so a
server that never sets it asks no one anything and the page it serves has
nothing new to say; #956 is a notice, not a reason to make `serve` phone
home for everyone.

The check asks for the latest release's version once at start, after the
listener is bound, and then once a day, and stops with the server. The
answer is kept in the server's state so any client can read it without
asking GitHub again, and a failed check keeps the last good answer and
says `last_check_failed`, so a notice doesn't come and go with the
network. `update_available` compares that version to the running one with
the comparison `ganymede update` already uses, `+build` suffix aside.

The HTTP stays in `ganymede-cli`, which already has `ureq` and the
release lookup `ganymede update` uses. The server crate gets a function,
`UpdateCheck`, that returns the latest version — `ganymede-cli` hands it
over in `Startup.update_check` — so `ganymede-server` gains no HTTP
client and no dependency. An app embedding the server passes none, and
the check stays off even with `check_for_updates` on: the Mac app updates
with Sparkle, so only the `ganymede` binary checks. `ganymede mcp`'s
private server passes none either.

A build of `main` is not checked. Its next release is the latest main
build, whose version is only known once it is downloaded, as
`ganymede update --check` says. The binary passes the version it prints
(`GANYMEDE_VERSION`, so `0.18.0+main.<commit>` on a build of main) in
`Startup.running_version`; the server's own crate version is the same for
a release build, and is what an embedded server uses.

The endpoint answers `enabled`, `running`, `latest`, `update_available`,
`checked_at` (RFC 3339, UTC), `last_check_failed` and `main_build`, and
needs the token like the other API routes. Tests inject a fake check, so
none of them touch the network. `docs/reference.md`, `docs/api.md`,
`docs/security.md` and `docs/privacy.md` cover the flag, the key, the
response and what the request carries.

The issue asked that a main build compare against the latest main build.
It doesn't, for the reason above: there is no version to compare until
the build is downloaded, so `/api/update` says `main_build: true` and
`ganymede update` remains the way to get a newer one.

Tests: `update.rs`'s unit tests cover the answer, a failed check, main
builds, and, with paused time, a second check a day after the first and
none before. `tests/embedded.rs` runs `run_with` with a counting check:
with the setting off the check is never called and `/api/update` says
`enabled: false`; on, the fake version comes back over HTTP. The CLI's
`serve`, `lab` and `ui` share `update::server_startup()`, which a test
checks hands over the check and `GANYMEDE_VERSION`. The CLI's own copies
of `is_main_build` and `base_version` are gone; it uses the server
crate's.
