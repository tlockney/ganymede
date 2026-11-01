## 2026-11-01, Line coverage in the job summary (#255)

A new `coverage` job in `ci.yml` runs `cargo llvm-cov --workspace` on
Linux with cdp-kernel, ipykernel and jupyter_server installed as in the
e2e job, so the end-to-end tests count. The ganymede binaries those
tests start are instrumented too: `cargo llvm-cov` builds the workspace
binaries with coverage on and sets `LLVM_PROFILE_FILE` for every child
process. A server stopped with SIGKILL writes no profile, so its lines
can read as missed.

`scripts/coverage-summary.mjs` turns the JSON report into the job
summary: the total, a table of changed `.rs` files (the diff against the
pull request's base), and a table by crate. Nothing fails on a number;
the reviewer reads the changed-files table to see what new code no test
ran.

`cargo-llvm-cov` comes from `taiki-e/install-action`, already pinned for
`cargo-about`, so no new action. The job is a separate one so e2e keeps
running uninstrumented binaries.
