## 2026-10-18, release hardening (#61)

A release shipped unstripped binaries without LTO, its only integrity
check was a `.sha256` fetched from the same place as the archive, the
cross-built x86_64 macOS binary never ran, and a release that failed
after its tag was pushed had no written way back. Now:

- `[profile.release]` with `strip = "symbols"`, `lto = "thin"`, and
  `codegen-units = 1`. The x86_64 Linux binary goes from 19.8 MB to
  12.8 MB, and a clean build takes about as long as before (2.5 min).
- The release job attests build provenance for every archive
  (`actions/attest-build-provenance`), and [install](../install.md#verify-a-download)
  says what the checksum does and doesn't prove and how to run
  `gh attestation verify`.
- The x86_64 macOS binary runs under Rosetta on the arm64 runner, so it
  gets the same `--version` check as the native ones.
- [Releasing](../releasing.md) is the recovery procedure: rerun, untag
  and retag (`tag-release.yml` now has `workflow_dispatch` for that,
  main only), or cut a patch release once a release is public.
- Install documents Gatekeeper (what to do after a browser download) and
  musl (no build yet; install from source).

Not done here: `site/install.sh` still picks the glibc build on musl and
still exits non-zero when `cdp-kernel install` fails after `ganymede` is
in place. Both are changes to the install script.
