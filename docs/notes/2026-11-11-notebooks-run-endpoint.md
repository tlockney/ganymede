## 2026-11-11, POST /api/notebooks/run (#652)

The run contract's HTTP front door, for the apps' `ganymede://run` links
and Shortcuts (ganymede-ui#303), which reach the in-process server over
loopback REST. Parameters are set when a kernel starts, so a
parameterized run can't use the kernel of a notebook already open; the
endpoint starts its own.

It starts that kernel through the kernel manager, not a provisioner of
its own, so it shows in `/api/kernels`, gets the host API comm, and can
be interrupted like any other. `KernelManager::start_with_env` keeps the
extra variables on the kernel, so a restart from a client still has the
parameters. A drop guard shuts the kernel down, which covers a client
that disconnects: axum drops the handler's future.

`PARAMS_ENV`, `params_env` and `record_params` moved from the CLI into
`ganymede-nbformat` beside `named_results`, so the CLI and the endpoint
share them. The body is one `--results` line, `error` a string as there.

Left out: `save: "copy"`. The host API note says copies go "beside the
run contract's other copies", a place no code defines yet; it comes
with the run ledger. The fake kernel gained a `result <name> <json>`
cell for the test.
