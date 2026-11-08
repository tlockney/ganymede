## 2026-11-08, a timeout while queued has its own error (#576)

Since #559, `execute_with` interrupts a timed-out cell only if the kernel
had started it, but both cases returned `Host(Timeout)`. A queued request
still runs when its turn comes, so a caller reporting "timed out" was
misleading. `ganymede_engine::Error::Queued(Duration)` now marks that
case; its message says the kernel will still run the code, and the CLI
prints it as is. Adding a variant to the public `Error` enum breaks
exhaustive matches in embedders; the crates are pre-1.0 and
`docs/embedding.md` lists the variants.
