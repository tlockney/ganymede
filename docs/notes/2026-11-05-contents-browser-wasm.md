## 2026-11-05, ganymede-contents builds for browser wasm (#159)

- `ganymede-contents` failed for `wasm32-unknown-unknown` because
  getrandom 0.4 has no backend there without its `wasm_js` feature. The
  crate now turns that on for browser wasm only, with uuid's `js` and
  chrono's `wasmbind`, as `ganymede-protocol` and `ganymede-kernel-host`
  already do. Notebook signing keys and file ids come
  from the JS crypto API there.
- With contents fixed, `ganymede-api --no-default-features` builds for
  browser wasm too. CI's "Core libraries for browser wasm" step now
  checks engine, api and contents.
- The only other wasm warning was an unused `Duration` import in
  `fs.rs`, used only under `cfg(unix)`; the import moved there.
