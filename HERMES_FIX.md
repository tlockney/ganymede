# Fix round: kernel provider socket (#983)

Branch `claude/project-thread-riqt1l` already holds the implementation
(`crates/ganymede-server/src/kernel_providers.rs`,
`crates/ganymede-server/tests/kernel_providers.rs`, docs). A review found
the items below. Fix every one, in this order. Line numbers are
approximate. Commit as you go (small commits). Don't push. Don't launch
any app, take screenshots or run `defaults`. Don't touch
`docs/reference.md`'s "These nine keys" paragraph (already fixed).

Budget: stop exploring early; each item names its file. Run only the
kernel_providers tests while iterating
(`cargo test -p ganymede-server --test kernel_providers` and
`cargo test -p ganymede-server --lib kernel_providers`), and the full
commands at the end once.

## Tests that are missing (each must fail when the guarded line is removed; check that by removing it once, then restore)

1. **Owner guards on `deliver` and `ended`** (`kernel_providers.rs` ~703 `started.contains`, ~737 `started.remove`). Integration test with two connections A and B: A registers and starts a kernel; B sends `deliver` and `ended` for A's kernel id. Assert A's kernel is still alive (`/api/kernels/<id>` not dead) and B's injected message does not show up as the execute output.
2. **Full queue disconnects** (~614-621, `self.close()` on `try_send` failure). Make the queue capacity injectable through the existing `Timing` (or a `pub(crate)` field beside it), default 1024. Test: capacity small (e.g. 4), a provider that never reads, a long `write_stall` so only the queue can trip; drive sends (executes) past the limit; assert the kernel ends dead with reason `lost`.
3. **Refusals at their call sites in `start`/`listen`** (~218 embedded, ~225 token None, ~251 `check_private_dir`, ~255 `sun_path`). Under the test file's `serial()` lock, call `kernel_providers::start` directly: (a) runtime dir chmod 0777 (restore after), (b) `JUPYTER_RUNTIME_DIR` pointed at a path long enough to exceed the limit, (c) token `None`, (d) `embedded = true`. Each: returns `None` and no socket file exists. Expose what's needed as `pub(crate)` or `#[doc(hidden)] pub`.
4. **`serve.rs` info file wiring**. Add a CLI test in `crates/ganymede-cli/tests/cli.rs`, modelled on the existing test near line 729 that spawns `ganymede serve` with its own runtime dir and reads `jpserver-<pid>.json`: with a config file holding `[kernel_providers]\nenabled = true`, the JSON has `kernel_providers_socket`, the path exists, and a `hello` with the token over a `UnixStream` gets `welcome`. Without the config, the key is absent. `#[cfg(unix)]`.
5. **`started` with `error`** (M17): a test where the fake answers `start` with `{"op":"started","kernel_id":..,"error":"no"}`; the kernel start through `/api/kernels` fails promptly (well under the startup timeout) and is not retried.
6. **Listener drop closes live connections**: drop the `Listener` guard (or stop the test server) with a connected provider that started a kernel; assert the provider's stream hits EOF and the socket file is gone.
7. **Interrupt on control and `JPY_SESSION_NAME`**: extend the happy-path test to assert the interrupt arrives with `channel == "control"`, and that the `start` frame's `launch.env.JPY_SESSION_NAME` is present when a session is created through `/api/sessions` (or drop the claim from the docs if not reachable; say which in the dev log).
8. **`Startup::kernel_provider_timing`**: either use it from a test (items 2 or 4) or remove it and the dev-log sentence about it.
9. **Race in `registrations_are_first_come…`**: after the non-holder's `unregister`, do a round trip on that connection (e.g. a `register` of a fresh name and wait for its `registered`) before asserting the spec is still listed.
10. **`a_spec_keeps_only_display_fields`** builds its own `Kernelspec`. Extract `fn display_spec(..) -> Kernelspec` used by `handle`, and test that function instead.

## Code fixes

11. **Remove both `unsafe` blocks** (~345 `libc::geteuid`, ~353 `mem::zeroed::<sockaddr_un>`). Own uid: use `std::os::unix::fs::MetadataExt::uid()` of the private temp dir this process just created (or of a file it created). `sun_path` length: a per-target constant (104 on macOS and the BSDs, 108 on Linux and Android) with `#[cfg(target_os = ...)]`. If `libc` is then unused in ganymede-server, remove it from Cargo.toml and Cargo.lock. Make the owner check injectable (`check_private_dir_for(dir, uid)`) and add a unit test for another owner.
12. **Staged bind path longer than the checked path** (~252-269): the check must cover the path actually bound (`<dir>/.ganymede-kernels-XXXXXX/socket`). Use a short staging dir name (e.g. `.gk-XXXXXX` and `s`) and check the longer of the staged and final paths.
13. **Race between a launch and close** (~490-493 and `ProviderKernel::start`): set `closed` under the same state lock the close path uses, and have `start` return `Err("provider disconnected")` when closed, checked under that lock, so a late start never leaves a pending `started` sender.
14. **Windows warning**: in `serve.rs` `run_with`, `#[cfg(not(unix))] if cfg.kernel_providers.enabled { tracing::warn!("the kernel provider socket is Unix only; not opened") }`, matching what docs/security.md and the dev log say.
15. **Duplicated JSON helpers**: `message_to_json`/`message_from_json` copy `ganymede-ffi/src/lib.rs` `message_json`/`parse_message`. Move one copy to a shared place both crates already depend on (ganymede-protocol), and use it from both.

## Docs

16. `docs/reference.md`: add the Unreleased note (text below) right under the `[kernel_providers] enabled` row's table, or as a sentence-level note after the table naming `[kernel_providers]`; and fix the row: the code refuses a group- or world-writable runtime dir or one owned by another user, not "isn't owner-only" (0755 is accepted). Say that.
17. `docs/kernel-provider.md`: an Unreleased note by the status line; remove "only the embedded server has one now" (stale); where it says the reservation keeps another process from claiming the app's kernel, say the reservation is not built yet (#986).
18. Dev log `docs/notes/2026-11-27-kernel-provider-socket.md`: make every claim true of the code after this round (the "another owner" test now exists via item 11; tests list; Windows warning; timing hook; events socket is #978/#980, use "#980" consistently with docs/api.md). Add a short "Unsafe and dependencies" line: no `unsafe`, and whether `libc` was dropped.

Unreleased note text (keep `**Unreleased.**` exactly):

```md
> [!NOTE]
> **Unreleased.** Available only from a build of `main` (HEAD), not in
> the latest release.
```

## Finish

Run, all must pass:

```
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
RUSTDOCFLAGS=-D warnings cargo doc --workspace --no-deps
```

Then `git rm HERMES_FIX.md` and commit.
