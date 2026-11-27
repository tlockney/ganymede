## 2026-11-27, chrono out of the dependency tree (#855)

Ganymede's own crates moved to jiff on 2026-11-17, but chrono still came
in through rmcp 3.11 (directly, and through schemars' `chrono04`), so
deny.toml kept an ignore for RUSTSEC-2026-0543. rmcp 3.12 makes chrono
an optional feature (`chrono`, on in its defaults) and depends on jiff.
ganymede-server already takes rmcp with `default-features = false` and
lists its features, so moving to 3.12 drops chrono, and with it
iana-time-zone, from Cargo.lock: `cargo tree -i chrono` finds nothing.
The advisory ignore is gone, so `audit` fails again if anything brings
chrono back.

The bump also brings rmcp 3.12's own fixes to `/mcp`: a malformed
Streamable HTTP body gets a 400, cache hints are left out for peers on
older protocol revisions, and unknown capability fields are kept. Every
`#[tool]` here sets its description explicitly, so 3.12's doc-comment
fix changes no tool text.
