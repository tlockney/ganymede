## 2026-11-25, Note that the apps fetch DuckDB on first use (#951)

ganymede-ui#794 stops fetching DuckDB's `.wasm` and worker when the page
first goes online: they load the first time a SQL cell, the DuckDB
console or a Parquet view needs them, or from Settings > Offline SQL in
a browser. docs/data-output.md gets an `**Unreleased in ganymede-ui.**`
note saying so; ganymede-ui's release lists it, and the text around it
is updated then.
