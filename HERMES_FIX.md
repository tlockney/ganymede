# Fix round: tlockney/ganymede#984

The commit "Kernel provider groundwork (#984)" (0f9f1dc) had a pre-push
review. Fix every item below. Amend nothing: make ONE new commit,
message "Address review of the kernel provider groundwork (#984)", that
also deletes this file. Don't push. Same rules as before: only cargo and
git, never launch apps, `defaults` or screenshots. Each new test must
fail when its fix is reverted (check by reverting briefly).

1. **Startup timeout, not a fixed 60 s.** Add
   `pub startup_timeout: Option<Duration>` to `LaunchOptions`
   (crates/ganymede-kernel-host/src/process.rs) with a doc comment. Fill
   it from `KernelManagerConfig.startup_timeout` wherever ganymede-api
   builds `LaunchOptions` for a launch (kernels.rs, `Core::launch` and any
   relaunch path). In registered.rs, the ack wait uses
   `opts.startup_timeout.unwrap_or(START_ACK_WAIT)`; keep START_ACK_WAIT
   as the fallback and update its doc. Test: a deferred start with
   `startup_timeout: Some(5 s)` under `start_paused` times out at 5 s,
   not 60 (for example advance 6 s and assert the error).

2. **A dropped launch cleans up.** If the `launch` future is dropped while
   it waits for the ack (a client disconnects), the run stays in
   `running`/`unconnected` and the app's kernel is never stopped. Add a
   drop guard created after the inserts, disarmed on success, whose Drop
   calls `abandon(&id, &process)` and `app.stop(&id)`; the explicit
   failure branches may keep their own handling but must not double-stop
   (disarm before returning, or let the guard do it, and keep the
   existing tests' stop counts true: a failed ack is not stopped, a
   timeout is stopped once). Test
   `a_launch_dropped_while_waiting_for_its_ack_leaves_nothing_and_stops`:
   spawn the launch, wait for `ack()`, abort the task, await it, then
   assert running and unconnected are empty and stops == 1.

3. **Test `boot`'s retry check.** In ganymede-api, add a test where the
   provisioner's process reports `restartable() == false` and
   `exit_reason() == Some("lost")` and dies before answering
   `kernel_info` (so the start fails with KernelDied). Assert the
   provisioner was launched exactly once (count launches), the start
   fails, and nothing is retried. Reuse the GoneProcess pattern in
   sessions.rs or kernels.rs tests.

4. **Test the dropped-sender branch.** registered.rs test
   `a_deferred_start_whose_sender_is_dropped_fails_and_leaves_nothing`:
   take the sender from `ack()`, drop it, assert an error and that
   running and unconnected are empty.

5. **Test `end_all_from` clearing `unconnected`.** Launch a provider's
   kernel (no connect), call `end_all_from`, assert `unconnected` is
   empty and the launched process is not alive.

6. **A provider kernel is never relaunched through the fallback.** Keep
   a record of the kernel ids started from a provider spec (for example
   `provider_runs: HashMap<String, String>` id -> name in State),
   removed when the host stops that kernel (`kill_now`) or when
   `end_all_from` ends it. In `launch`, when `opts.kernel_id` is such an
   id: launch through the provider that registers the name now if one
   does (Origin::Provider), otherwise fail with
   `Error::NoSuchKernelspec(name)`; never the fallback, and never an
   App-origin spec. This matches docs/kernel-provider.md ("a name a
   connection registered and then lost is never launched through the
   fallback"). Test
   `a_provider_kernel_is_not_relaunched_through_the_fallback`: provider
   registers "app", launch under "k1", unregister "app", fallback lists
   "app" (use StaticSpecs + Never), relaunch under "k1" fails with
   NoSuchKernelspec and Never is not called (assert the error variant,
   not Unsupported("never")). A launch of "app" with a new id goes to the
   fallback as before.

7. **Docs on `AppState.registered`** (crates/ganymede-server/src/lib.rs):
   reword to say `run_with` always sets it; `LocalHost` and tests leave
   it None.

8. **`RegisteredKernels` struct doc** (registered.rs ~line 95): say an
   app's specs are listed after the fallback's and in place of a
   namesake, and a provider's after everything else, never in place of
   one.

9. **Dev log wording** (docs/notes/2026-11-27-kernel-provider-groundwork.md):
   replace "is listed and launched after everything else" with "is
   listed after everything else and never launched in place of a
   fallback's namesake". Add one sentence on items 1, 2 and 6 (the ack
   waits up to the startup timeout, a dropped launch stops the kernel, a
   provider kernel is never relaunched through the fallback). Keep it at
   most 8 sentences.

Checks before committing:

    cargo fmt --all
    cargo clippy --workspace --all-targets -- -D warnings
    cargo test -p ganymede-kernel-host -p ganymede-api -p ganymede-server -p ganymede-ffi
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
