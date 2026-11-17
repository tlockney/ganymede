## 2026-11-17, chrono unmaintained advisory (#856)

The 2026-11-17 Dependabot sweep found RUSTSEC-2026-0543: chrono is
unmaintained, every version, no fixed release. deny.toml fails on
unmaintained advisories, so the `audit` job would fail on every push to
main, the weekly run and any PR that changes dependencies. It is not a
vulnerability, and chrono is a direct dependency of seven crates, so the
port is its own issue (#855, jiff or time). Until then deny.toml ignores
the advisory with a revisit date of 2027-02-17.

The same sweep moved clipboard-win from the yanked 5.4.1 to 5.4.3 (via
rustyline, Windows only), a warning earlier sweeps carried over.
