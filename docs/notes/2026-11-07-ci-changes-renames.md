## 2026-11-07, renames in ci-changes.sh (#464)

`scripts/ci-changes.sh` listed changed files with `git diff --name-only`,
which reports a rename by its new path only. Moving a code file under
`docs/` reported `code=false`, so the change skipped the test jobs.
`--no-renames` lists both paths, and the removed code path counts. The
same fix went into cdp-kernel, ganymede-ui and ganymede-device.
