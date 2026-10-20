## 2026-10-20, iopub_welcome ends the startup nudge (#143)

ipykernel 7 binds iopub as XPUB and, per JEP 65, publishes `iopub_welcome` when a subscription reaches it. The session already counted any iopub message as "iopub is live", so the welcome was ending the nudge after one `kernel_info_request` on each channel; it was also being fanned out to clients like kernel output. It now stops in the session: the welcome answers the host's single shared subscription, which no client made. jupyter_server forwards it only because each websocket there opens its own SUB socket, and `@jupyterlab/services` has no handler for it. PUB kernels (cdp-kernel, older ipykernel) keep the existing fallback: requests with backoff until one's status gets through.

Checked against ipykernel 7.4.0: with `RUST_LOG=ganymede_kernel_host=debug`, `ganymede run -k python3` logs the welcome before the first cell. The pure-Rust `zeromq` SUB socket's subscription is what triggers it.

Lesson: the welcome carries no parent and no state, so it must not touch `execution_state`; the API layer's starting-to-idle rule after the nudge still decides that.
