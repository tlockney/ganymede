# jupyter-core-rs

A sketch of what a Rust reimplementation of the Jupyter core would take:
a kernel host, a web presentation layer, and the messaging protocol.

- [`sketch.md`](sketch.md): components, what already exists in Rust, the
  parts that are usually missed, and a suggested order of work.
- [`notes.md`](notes.md): findings and open questions.

## Status

Design sketch. Nothing built.

## Findings

- The server half (protocol, kernel lifecycle, nbformat, REST and
  websocket server, CRDT document) is well-specified and suits Rust.
- The frontend half does not: editor, math, output renderers, and widgets
  are JavaScript. A TypeScript UI over a Rust server is the pragmatic shape.
- The runtimed crates (`jupyter-protocol`, `runtimelib`, `nbformat`)
  already cover protocol, kernel client, and file format. No Rust
  equivalent of `jupyter_server` exists.

## Next

Resolve the open questions in `notes.md` (product vs. understanding,
widgets, Windows, compatibility with existing clients), then start with
a protocol crate and a CLI that runs one cell on ipykernel.
