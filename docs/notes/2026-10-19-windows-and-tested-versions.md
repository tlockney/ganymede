## 2026-10-19, Windows install, tested versions, the log out of search (#184, #193, #192)

- The Windows steps on the Install page were checked against v0.3.0's
  assets: the `.zip` holds `ganymede-<version>-x86_64-pc-windows-msvc/ganymede.exe`,
  and its `.sha256` is `<hash> *<name>` (binary mode), so the first
  space-separated field is the hash. PowerShell's `-ne` compares strings
  case-insensitively, so `Get-FileHash`'s upper-case hash matches. The
  PowerShell itself was not run; CI has no Windows job for it (#60).
- The tested-versions table in Architecture takes every version from
  `ci.yml`'s pins, `ganymede lab install`'s default, and
  `validation/package-lock.json`. Keep it in step when they're bumped.
- Site search now leaves the development log out. Facts that lived only
  in entries were checked: the request-body limit is `max_body_size` on
  the reference page, and chunked uploads, the kernel's working
  directory and outside-root 404s follow `jupyter_server`, so they need
  no page of their own.
