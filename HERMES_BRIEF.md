# Brief: reserve kernel provider names by code signature (tlockney/ganymede#986)

Branch `claude/project-thread-riqt1l`, from `main` (the socket from #983 is merged). Read first: `crates/ganymede-server/src/kernel_providers.rs` (accept path, `handle` for `register`, `Timing`, tests module), `crates/ganymede-server/tests/kernel_providers.rs`, `crates/ganymede-server/src/config.rs` (`KernelProvidersConfig`), `docs/kernel-provider.md` ("Names are first come", status note), `docs/security.md` ("Kernel providers"), `docs/reference.md` (`[kernel_providers]` row), `docs/embedding.md` ("Kernel providers").

This Mac is macOS, so the macOS-only code and tests run here. Work in small commits. Don't push, launch apps, take screenshots or run `defaults`. Budget your turns: don't re-read files you've already read.

## Design

1. **Config.** `KernelProvidersConfig` gains `reserve: BTreeMap<String, String>`: kernelspec name → a macOS code requirement string (the `csreq` language, e.g. `identifier "net.lockney.Ganymede" and anchor apple generic and certificate leaf[subject.OU] = "ABCDE12345"`). Default empty. Names are lowercased on load, as `register` lowercases names.
2. **Peer identity, macOS.** On accept (where `peer_cred` is read), also read the peer's audit token with `getsockopt(fd, libc::SOL_LOCAL, libc::LOCAL_PEERTOKEN, &mut [u8; 32], &mut len)` and keep it on the connection (`Option<[u8; 32]>`; `None` if the call fails, logged at `warn!`). This is `unsafe`: keep it in one small function, `#[cfg(target_os = "macos")]`, with a SAFETY comment (a 32-byte buffer and its length, an fd the stream owns for the call's duration). Add `libc = "0.2"` to ganymede-server only under `[target.'cfg(target_os = "macos")'.dependencies]`.
3. **The check, macOS.** Add `security-framework = { version = "3", default-features = false, features = ["OSX_10_15"] }` under the same macOS-only dependencies (it is already in Cargo.lock at 3.7.0 through rustls-platform-verifier, so no new crate enters the build; check `Cargo.lock` gains no new package and say so in the dev log). A function `fn peer_meets(token: &[u8; 32], requirement: &str) -> Result<(), String>`: parse with `SecRequirement::from_str`, build `GuestAttributes` with `set_audit_token(CFData::from_buffer(token))`, `SecCode::copy_guest_with_attribues(None, &attrs, Flags::NONE)`, then `check_validity(Flags::NONE, &req)`. Any error → `Err` with the reason. Never use the pid.
4. **The rule, at `register`** (after the name is lowercased and validated, before `RegisteredKernels::register`):

| Case | Result |
| --- | --- |
| name not in `reserve` | unchanged (first come) |
| reserved, not macOS | refused: `registered` with `error` "kernelspec <name> is reserved for another program" |
| reserved, macOS, no audit token | refused, same error |
| reserved, macOS, requirement fails to parse | refused, same error; also `warn!` once per bad requirement at startup (parse each requirement when the listener starts and log the bad ones) |
| reserved, macOS, peer doesn't meet it | refused, same error (log the Security error at `debug!`) |
| reserved, macOS, peer meets it | goes on to `register` as before (a name on disk is still refused there) |

5. Keep the check a pure-ish function so it can be tested: `fn reservation_allows(reserve: &BTreeMap<String,String>, name: &str, token: Option<&[u8; 32]>) -> Result<(), String>` with the platform split inside.

## Tests

- Unit (all platforms): a name not reserved is allowed with `token: None`; a reserved name with `None` is refused.
- Unit (`#[cfg(not(target_os = "macos"))]`): a reserved name is refused even with `Some(token)`.
- Unit (`#[cfg(target_os = "macos")]`): get this process's own audit token with `task_info(mach_task_self(), TASK_AUDIT_TOKEN, ...)` (see security-framework's own test `copy_kernel_guest_with_launch_token`-style code; or connect a `UnixStream::pair()` and read `LOCAL_PEERTOKEN` with the new function, which also tests that function: prefer this). Then: requirement `always` → allowed; `never` → refused; `identifier "no.such.app" and anchor apple generic` → refused; an unparsable requirement (`not a requirement((`) → refused.
- Integration (`tests/kernel_providers.rs`): with `reserve = {"cdp-browser" = "never"}` passed into `start` (thread the map through `kernel_providers::start`; it already takes the enabled flag, timing and registered set), a provider registering `cdp-browser` gets `registered` with an `error` containing "reserved", and `/api/kernelspecs` doesn't list it; registering `cdp-node` on the same connection succeeds. On macOS only, a second test with `"always"` succeeds.
- Each guard must fail a test when removed: check by removing the reservation call in `handle` once, run, restore.

## Docs

- `docs/kernel-provider.md`: "Names are first come" describes the reservation as built: the requirement string, the audit-token check (not the pid), refused to everyone off macOS, refused on any error. Drop "(#986)" pending wording; keep the Unreleased note.
- `docs/security.md` "Kernel providers": one bullet for reservations.
- `docs/reference.md`: a row for `[kernel_providers] reserve` with an example, beside the `enabled` row (the existing Unreleased note covers the table; update the "These nine keys" count to ten if that paragraph counts table rows: read it).
- `docs/embedding.md` "Kernel providers": one sentence on the refusal text.
- Dev log `docs/notes/2026-11-28-kernel-provider-reserve.md` starting `## 2026-11-28, kernel provider name reservation (#986)`: design, the `unsafe` block and why it is sound, the two macOS-only dependencies and that Cargo.lock gains no package, what the tests cover on each platform. Don't edit docs/notes.md.

## Finish

All must pass here:

```
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p ganymede-server
RUSTDOCFLAGS=-D warnings cargo doc -p ganymede-server --no-deps
```

Then `git rm HERMES_BRIEF.md` and commit.
