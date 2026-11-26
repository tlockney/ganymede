# Brief: tlockney/ganymede#963 — gateway lists local kernels too, and reports its status

Read the issue first: https://github.com/tlockney/ganymede/issues/963 (design + acceptance criteria).
Follow /Volumes/Secondary/src/personal/ganymede-server CONTRIBUTING.md and docs/development.md for build/test rules.
Work on this branch (`claude/project-thread-obabcr`). Commit your work; do not push. Delete this HERMES_BRIEF.md in your last commit.

## Where things are
- `crates/ganymede-server/src/serve.rs`: `fn gateway(cfg)` (~line 465) builds `ganymede_gateway::Gateway`; ~line 784 picks `(specs, provisioner, transports)` from the gateway OR local (`FsKernelspecs::discover`, `default_provisioner()`, `default_transports()`); then `RegisteredKernels::set_fallback` wraps them.
- `crates/ganymede-kernel-host/src/registered.rs`: `RegisteredKernels` already routes list/launch/connect between its own specs and a fallback. Copy that pattern for a combined gateway+local source.
- `crates/ganymede-gateway/src/lib.rs`: `Gateway::refresh`, `refresh_every` (failures only logged), `KernelspecSource`/`Provisioner`/`TransportFactory` impls; `handle_key` maps a launched process to a remote kernel id.
- `crates/ganymede-api/src/models.rs`: `KernelspecsModel`, `KernelspecModel` (see how `resource_dir` was added as an optional extra key, #891). `crates/ganymede-api/src/kernels.rs:551` builds the model.
- `crates/ganymede-server/src/config.rs`: `gateway_url`, `gateway_token`; add `gateway_name: Option<String>`.
- `crates/ganymede-ffi/src/lib.rs` `startup()`: with a gateway configured it returns `Startup::default()`. A build without zmq/subprocess (iOS) must keep gateway-only behavior.

## What to build
1. A combined kernels source (new small module, e.g. `crates/ganymede-server/src/combined.rs` or in ganymede-gateway): list = gateway specs, then local specs whose names (case-insensitive) the gateway lacks. `launch` routes by which side lists the name; `connect` routes to the side that launched it (track launched process keys as `Gateway`/`RegisteredKernels` do).
2. Use it in `serve.rs` only when a gateway is configured AND local kernels are available (local provisioner/transports build without error, i.e. `zmq`+`subprocess`, or the Startup supplies them). Otherwise keep today's behavior exactly. `Startup.kernelspecs/provisioner/transports` from an app still win as now.
3. `Gateway` records its last refresh outcome (reachable bool, error string, time). While unreachable, `refresh_every` retries every 15 s; 60 s while it answers.
4. API: `/api/kernelspecs` adds optional top-level `"gateway": {"name", "reachable", "error"}` (only when a gateway is configured; name = `gateway_name` else the URL's host; never the URL path/query/token) and per-spec `"gateway": true` for gateway-served specs (skip_serializing_if false/None). Thread the info through `KernelManager` in the least invasive way (e.g. a field on `Kernelspec` marked `#[serde(skip)]` for origin, and an optional status provider on the manager).
5. Tests: listing union + clash (gateway wins), launch/connect routing, unreachable gateway → local specs listed and `reachable:false` with error, recovery flips it back, no token in API output. Use existing gateway test helpers (look for a fake remote server in ganymede-gateway / ganymede-server tests).
6. Docs: docs/kernels.md (gateway section), docs/reference.md (config key `gateway_name`, API fields), each new section with the "**Unreleased.**" note from CLAUDE.md. Dev log: `docs/notes/2026-11-26-gateway-local-kernels.md` starting `## 2026-11-26, gateway lists local kernels (#963)`. Never edit docs/notes.md.
7. Run `cargo fmt`, `cargo clippy --all-targets -- -D warnings` and the tests for the crates you touched; regenerate docs/reference.md / docs/mcp-tools.md if a check requires it. `cargo clean` is fine if disk runs low.

Report: files changed, test commands run and their results, anything you couldn't do.
