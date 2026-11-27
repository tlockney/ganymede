# Brief: write events on the events socket (#978)

Implement tlockney/ganymede#978 exactly. The issue body is copied below and
is the spec. Make ONE commit titled "Report the server's file writes on the
events socket (#978)" that also deletes this file.

Pointers:
- crates/ganymede-server/src/write_observer.rs: add the broadcast channel
  next to the FFI function. `is_set` becomes "FFI function set OR the
  channel has a receiver". `notify` gets the contents-API path too, and
  sends the event to the channel (ignore a send error with no receivers).
  Keep the FFI callback's canonical absolute path unchanged.
- crates/ganymede-server/src/stamping.rs: `Stamps::observe(path)` already
  has the API path; carry it in `Observed` and pass it to `notify`.
- crates/ganymede-server/src/serve.rs ~964: always build a WriteObserver
  (use `startup.write_observer` when given, else a new one), give it to
  collab, and keep it in AppState so the events socket can subscribe.
  Check every other place that builds Collab/Stamps with an observer.
- crates/ganymede-server/src/ws.rs `events`: subscribe, forward each event
  as a JSON text frame in jupyter_events' envelope; on
  `RecvError::Lagged` close with 1011 reason "missed events"; keep
  handling the client's Close. Use `chrono` or `time` only if already a
  dependency of ganymede-server; no new dependencies.
- Tests: crates/ganymede-server/tests/ (see http.rs for websocket tests)
  for each acceptance item, including lag (a tiny channel capacity via a
  test-only constructor is fine) and "no stamp without a listener".
- Docs: docs/api.md (the events socket entry and a short section on the
  write event: schema id, fields, close code) with the Unreleased note
  from CLAUDE.md; docs/embedding.md one sentence pointing non-embedding
  clients to it; dev log docs/notes/2026-11-27-write-events.md starting
  `## 2026-11-27, write events on the events socket (#978)`. Never edit
  docs/notes.md.

Before committing, all must pass:
  cargo fmt --all --check
  cargo clippy --workspace --all-targets -- -D warnings
  cargo test -p ganymede-server -p ganymede-ffi

---

Part of #965, section 3 ("Write events").

## Why

The Mac app keeps answers per notebook file: a link's "Always allow", the device ledger's answers, and folder grants. They follow the file when Ganymede writes it, and are forgotten when something else changes it (#584, #916). Today the app learns of the server's writes through `ganymede_server_set_write_observer`, which only works for the server it embeds. Once local notebooks go through a separate `ganymede serve` (#965 section 4, and This Mac's own server from ganymede-ui#883), the app needs the same reports over the API.

## Design

Use the socket that already exists, so no new route is added. `/api/events/subscribe` (`jupyter_events`' stream, which JupyterLab already keeps open) emits nothing today. It will carry one event per server write: the same writes, after the same success, that the observer reports (`write_observer.rs` lists them).

- **Envelope:** `jupyter_events`' form: `__schema__`, `__schema_version__` (1), `__metadata_version__` (1) and `__timestamp__` (RFC 3339 UTC), plus the data fields.
  - Schema id: `https://ganymede-server.com/events/write/v1`.
  - Clients that don't know the schema ignore it. JupyterLab's event manager passes unknown schemas along without acting on them.
- **Data:**
  - `path`: the file's contents-API path, as the write named it, relative to the server's root and `/`-separated. It is never an absolute or canonical path, so a client sees nothing it couldn't already list. The app joins it with `root_dir` and resolves links itself.
  - `before` and `after`: the file's stamps (`ganymede_contents::file_stamp`), or `null`.
- **Who gets it:** every authenticated subscriber on the route as it stands, by token, cookie or tailnet, with origin checked. A machine mount already forwards `api/events/subscribe`. A token holder can already read and list every file, so the event adds no authority.
- **Delivery:**
  - A bounded `tokio::sync::broadcast` channel (capacity about 256), created by `serve` whether or not an embedding app passes a `WriteObserver`.
  - A subscriber that lags, and so missed events, has its socket closed with code 1011 and the reason `missed events`. A client then knows its view may be stale, and the app forgets the answers it can't confirm, which fails safe.
  - A write is stamped beforehand only when someone is listening: the FFI observer is set, or the channel has a receiver. Today's cost with nobody listening stays the same.
  - The FFI observer keeps working unchanged, with its canonical absolute path.

## Acceptance criteria

- [ ] A client on `/api/events/subscribe` gets one write event per save through the contents API, a room's autosave, a run's output save, a checkpoint restore and a copy to a named path, with correct `before` and `after`. A failed write, a rename, a delete, an untitled new file and a directory save send nothing.
- [ ] A lagging subscriber is closed with 1011 `missed events`. Other subscribers are unaffected.
- [ ] With no subscriber and no FFI observer, no stamp is taken before a write.
- [ ] The FFI observer's reports are unchanged, and the existing tests pass.
- [ ] docs/api.md documents the event, its schema and the close code, with an **Unreleased.** note. docs/embedding.md points to it as the way for a non-embedding client. Dev-log entry.
