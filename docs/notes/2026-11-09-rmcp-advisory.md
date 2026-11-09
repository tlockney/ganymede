## 2026-11-09, rmcp 3.9.0 for a RustSec advisory (#605)

A new advisory against rmcp 3.8.0 made the `audit` job fail on main and on
every open PR. Its streamable HTTP client read response bodies with no size
limit. Bumped rmcp and rmcp-macros to 3.9.0 in Cargo.lock only. A plain
`cargo update -p rmcp` with a newer local toolchain also moved
`windows-sys` and `getrandom` in a few dependency lists, so I kept just the
two package entries.
