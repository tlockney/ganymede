## 2026-10-18, rename without the check-then-rename race (#75)

The first #75 change left `rename` checking that the target was free and
then calling `fs::rename`, which replaces an existing file (and, on Unix,
an empty directory). A file created in between was lost.

- `rename` now asks the OS to refuse an existing target in the same call:
  `renameat2(RENAME_NOREPLACE)` on Linux and Android (the raw syscall, so
  glibc before 2.28 is fine), `renamex_np(RENAME_EXCL)` on macOS and iOS,
  through `libc`, which the contents crate now depends on for those
  targets. `EEXIST` is still jupyter_server's 409 "File already exists".
- Where the flag is not available (other platforms, a kernel or a
  filesystem that answers `ENOSYS`, `EINVAL` or `ENOTSUP`, such as some
  network filesystems) it falls back to check-then-rename, as before.

Still open: an opt-in ETag or `If-Unmodified-Since` precondition on
saves. That is a design decision, not a bug fix, so it is left for later.
