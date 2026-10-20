## 2026-10-20, Windows kernels run in a job object (#145)

On Windows the subprocess provisioner had no way to kill a kernel: `kill` returned `Unsupported`, so a shutdown ended with the kernel still running, and a kernel outlived the server. A kernel now starts with `CREATE_SUSPENDED`, goes into its own job object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, and is then resumed. `kill` terminates the job, which takes every process the kernel started, as SIGKILL to the process group does on Unix. The job is also terminated once the kernel exits, so leftovers go with it, and when the host exits its handle closes and the job's processes die. `terminate` stays `Unsupported` (no SIGTERM counterpart), so shutdown goes from `shutdown_request` straight to the kill.

`std` gives no handle to the suspended main thread, so the resume finds the kernel's threads in a Toolhelp snapshot, as the `command-group` crate does. If the job can't be set up, for example because the host is in a job that forbids nesting, the kernel is resumed anyway and runs without one.

Lesson: the code builds for Windows from Linux with `rustup target add x86_64-pc-windows-gnu` and `cargo clippy -p ganymede-kernel-host --target x86_64-pc-windows-gnu`; no C toolchain is needed for that crate. It has not been run on Windows: CI has no Windows test job (#60).
