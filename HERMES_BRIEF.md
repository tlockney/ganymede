# Brief: tlockney/ganymede#984, kernel provider groundwork

Read the issue text below, docs/kernel-provider.md (the design) and
docs/in-app-kernel.md (the C ABI it extends). Implement #984 only: no
socket, no config, no listener; that is #983, later.

Rules:
- One commit, message "Kernel provider groundwork (#984)", that also
  deletes this file. Don't push.
- Never launch apps, run `defaults`, change appearance or take
  screenshots. Only cargo and git.
- Keep the C ABI (`crates/ganymede-ffi`) behavior and its tests as they
  are. The ffi code may need small call-site changes for new parameters.
- Comments and docs: plain, short, say what and why; follow the style of
  registered.rs.
- Tests next to the code (`#[cfg(test)] mod tests`), as registered.rs
  and ganymede-api's sessions.rs tests do.

## What to build

### 1. Spec origin (registered.rs)

Add `pub enum Origin { App, Provider }` (doc: App = the embedding
process through the C ABI; Provider = a process connected over the
kernel provider socket, #983). `register` takes it (add a parameter;
update ganymede-ffi to pass `Origin::App`). Store it per spec.

- App: as today, listed after the fallback's and in place of a namesake;
  launched through the app even when the fallback lists the name.
- Provider: `register` fails with `Error::Registration` when the fallback
  lists the name (case-insensitive), and when any spec of that name is
  registered. In `list`, provider specs come after everything else and
  never replace a fallback spec (if the fallback lists the name by the
  time of `list`, drop the provider's from the list). In `launch`, a name
  the fallback lists goes to the fallback even if a provider registered
  it.

### 2. `unregister(&self, name: &str) -> bool`

Removes the spec of that name (any origin). True when one was removed.
Running kernels are not touched. A later launch of the name goes to the
fallback (or fails with NoSuchKernelspec without one).

### 3. Deferred start acknowledgement

Add a default method on `AppKernel`:

    /// For an app whose start finishes later: a receiver that yields
    /// Ok once the kernel is started or Err with why it couldn't be.
    /// None (the default) means `start`'s return already said so.
    fn started(&self, kernel_id: &str) -> Option<tokio::sync::oneshot::Receiver<Result<(), String>>> { None }

In `launch`, after `app.start` returns Ok, if `app.started(&id)` gives a
receiver, await it with a timeout. Use the startup timeout the host
already has for kernel_info if `LaunchOptions` carries one; otherwise
add a constant `START_ACK_WAIT = 60 s` with a doc comment. On Err, on a
dropped sender, or on timeout: clean up exactly as the existing failed
start path does (remove from running and unconnected), call
`app.stop(&id)` on timeout only, and return `Error::Registration` with
the message. No retry.

### 4. `Ended::Gone`

New variant: "The process that ran it went away (a provider's
connection closed). The kernel dies with reason `lost` and is never
restarted." `as_str` returns "lost" for it too, so clients see
`"reason": "lost"`. ganymede-api decides restarts from `exit_reason()`
== "lost" today (kernels.rs around lines 1020, 1154, 1230, and
sessions.rs): it must not restart a Gone kernel. Add to
`KernelProcess` (process.rs) a default method
`fn restartable(&self) -> bool { true }`, override it in registered.rs's
`Process` to return false when ended with Gone, and make ganymede-api's
restart decision check it (no restart, status dead, reason lost). Keep
today's Lost behavior (restart within its own limit) unchanged.

Also add `RegisteredKernels::ended(kernel_id, Ended::Gone)` support (it
already takes any Ended) and a helper `pub fn end_all_from(&self, app:
&Arc<dyn AppKernel>, why: Ended) -> usize` that ends every running
kernel whose app is that `Arc` (Arc::ptr_eq) and unregisters that app's
specs; returns how many kernels it ended. The socket will call it when a
connection closes.

### 5. `ganymede serve` builds a RegisteredKernels

In crates/ganymede-server/src/serve.rs (around line 910, `match
startup.registered`): when `Startup.registered` is None, create one,
set its fallback to the server's own specs/provisioner/transports, and
use it, exactly as the Some branch does. Keep it reachable for #983:
store the `Arc<RegisteredKernels>` where the server can hand it to the
socket later (for example a field in AppState, or return it from the
setup function); pick the smallest change and say in a comment it is
for #983. With nothing registered, lists and launches must be the same
as before.

## Tests (each must fail without its change; check by reverting briefly)

In registered.rs tests:
- `a_provider_cannot_take_a_name_the_fallback_lists`
- `provider_specs_list_after_the_fallback_and_never_replace_one`
- `an_unregistered_name_goes_to_the_fallback` and one without a fallback
  that fails with NoSuchKernelspec
- `a_deferred_start_waits_for_its_acknowledgement` (start resolves only
  after the receiver is sent Ok)
- `a_deferred_start_that_fails_leaves_nothing_running`
- `a_deferred_start_that_never_answers_times_out_and_stops` (use
  tokio::time::pause or make the wait configurable for tests)
- `end_all_from_ends_only_that_apps_kernels_and_unregisters_its_specs`

In ganymede-api (beside the existing "lost" tests in sessions.rs and/or
kernels.rs): a kernel whose process ends restartable() == false with
exit reason "lost" goes to `status: dead` with `reason: lost` and is not
restarted.

In ganymede-server: a test that `ganymede serve` with nothing registered
lists the same kernelspecs as before (if an existing test already
covers listing through serve, name it in your report instead).

## Dev log

Add docs/notes/2026-11-27-kernel-provider-groundwork.md:

    ## 2026-11-27, kernel provider groundwork (#984)

then 3 to 6 plain sentences: what changed and why (spec origin,
unregister, deferred start, Gone with no restart, serve builds a
RegisteredKernels), and that the socket itself is #983. Never edit
docs/notes.md.

## Checks before committing

    cargo fmt --all
    cargo clippy --workspace --all-targets -- -D warnings
    cargo test -p ganymede-kernel-host -p ganymede-api -p ganymede-server -p ganymede-ffi
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps

All must pass. Public docs must not link private items.
