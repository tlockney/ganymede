## 2026-10-20, the last cdp-kernel conformance scenarios (#69)

What PR #105 left for later, ported from cdp-kernel's `test/e2e.py` into the real-kernel tests:

- `debugger_stops_at_a_breakpoint` (kernels.rs): JupyterLab's sequence over control (`debugInfo`, `initialize`, `attach`, `dumpCell`, `setBreakpoints`, `configurationDone`), a cell stopped at line 2 with a `stopped` `debug_event` on iopub, `stackTrace`, `continue`, `disconnect`. Each `debug_request` waits for its reply and its idle, as JupyterLab does. Passes on cdp-kernel 0.7.1 and ipykernel 7.4.0. ipykernel 7 speaks protocol 5.5 and announces the debugger as `supported_features: ["debugger"]`, not `debugger: true`; the test accepts either, and a kernel offering neither is skipped (which the e2e job turns into a failure).
- `malformed_stdin_and_garbage_payloads_are_tolerated` (kernels.rs): while a prompt waits, a second DEALER on the kernel's stdin port sends a frame set with a non-JSON header; the kernel drops it and the real `input_reply` answers the prompt. On cdp-kernel, `__cdpk_send('not json')` from a cell leaves the kernel working. This needed `zeromq` and `bytes` as dev-dependencies of `ganymede-cli`.
- `parallel_starts_through_serve` (server.rs): four kernels started at once over REST, each answers over its own websocket.
- `restart_failure_on_a_real_kernel_reports_it_dead` (server.rs): a kernelspec wraps the installed one in `sh -c '[ -e "$FLAKY_MARK" ] && exit 3; : > "$FLAKY_MARK"; exec "$@"'`, so it runs once and every relaunch fails. A REST restart answers an error, the websocket sees `restarting` then `dead`, and the kernel is gone. The wrapper works for any kernelspec, so no kernel needed changing.
- CI job `e2e-cdp-kernel-main`, weekly and on `workflow_dispatch` only: clones cdp-kernel, `npm ci`, installs the `npm pack` tarball (prepack builds it), and runs `cargo test -p ganymede-cli` on cdp-node.

Kernel death through `serve` on a real kernel is covered by the crash tests in #144's PR.
