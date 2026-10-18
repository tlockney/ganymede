## 2026-10-18, install script: libc check and kernel failures (#61)

The last two items of #61, left over from the release hardening entry,
are in `site/install.sh`:

- On Linux, `target()` checks the C library before downloading.
  `getconf GNU_LIBC_VERSION` gives the glibc version, and anything older
  than 2.35 (what ubuntu-22.04 builds link) is refused with a pointer to
  `cargo install`. Without glibc, musl is recognised from `ldd --version`
  or `/lib/ld-musl-*`. Detecting was chosen over shipping a musl build;
  a static musl target is still open if Alpine users show up.
- A failing `cdp-kernel install` no longer fails the script after
  `ganymede` is in place. It prints what failed and the command to rerun,
  and on success prints `cdp-kernel version`, so an install log records
  which kernel went with which Ganymede.

`scripts/test-install.sh` covers both with stub `getconf`, `ldd`,
`node`, `npm`, and `cdp-kernel` in front of `PATH`. Stubs go in a
directory per case, since a shared one leaks a stub into the next case.
