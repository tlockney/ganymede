## 2026-11-22, url 2.5.9 for RUSTSEC-2026-0592 (#928)

RUSTSEC-2026-0592 came out on 2026-11-22: `Url::origin()` in `url`
2.5.8 and earlier recursed once per nested `blob:` prefix, so a 50 KB
URL overflowed a thread's stack and aborted the process. Ganymede had
2.5.8 through reqwest, sentry and tower-http. Its own code never calls
`Url::origin()`, but the bump is lockfile-only, so it was cheaper to
take than to prove every caller safe.

The advisory showed up first as a red `audit` job on Dependabot's
cargo group PR (#927), whose changes had nothing to do with `url`. The
`audit` job runs only when dependency files change, so a new advisory
blocks the next dependency PR rather than `main`. Taking the fix in its
own PR lets #927 rebase and be judged on its own changes.

`cargo update -p url` on its own reported "Locking 0 packages" with a
stale local index; `--precise 2.5.9` fetched it.
