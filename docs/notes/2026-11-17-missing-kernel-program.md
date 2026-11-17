## 2026-11-17, Name the kernelspec when its program is missing (#859)

A kernelspec whose `argv[0]` doesn't exist failed with `spawning "/path": No
such file or directory`, which didn't say which kernelspec, where its
`kernel.json` is, or what to do.

- `spawn_error` in `ganymede-kernel-host`'s `process.rs` builds the error when
  the spawn fails with `NotFound`: `kernelspec NAME (DIR/kernel.json) runs
  PROGRAM, which doesn't exist; reinstall the kernel or fix argv in
  kernel.json`. A bare name (no path separator) reads "runs `prog`, which
  isn't on PATH". A spec with an empty `resource_dir` (not loaded from disk)
  omits the parenthesised path.
- `NotFound` from a spawn has other causes, so it checks before blaming the
  program: a missing working directory (a notebook folder moved while its
  kernel ran, then a restart) is named as such, and a program that exists
  but whose `#!` interpreter or loader is missing says so.
- The `io::ErrorKind::NotFound` is kept, so callers that match on kind still
  work. The text goes out unchanged through `Error::Io` ("io: ..."), so the
  REST error body, `ganymede execute` and the apps show it as they showed the
  old one. Other spawn errors keep `spawning "prog": ...`.
- The text names no language or kernel: the host crates stay language-neutral.
- Windows: `NotFound` still comes from the (suspended) spawn, so the same path
  applies.
