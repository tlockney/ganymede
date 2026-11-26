# Brief: embedded servers take room locks (Closes #966)

Read CLAUDE.md, CONTRIBUTING.md and docs/development.md first. Tracking issue: #965.

## Problem
`crates/ganymede-server/src/serve.rs` (around line 858) calls `collab.with_room_locks(...)` only when
`startup.stop.is_none()`. Every embedding app passes `stop`, so the Mac app (ganymede-ffi) takes no
room lock, and it and a `ganymede serve`/`lab`/`ui`/`mcp` server can both edit and autosave one notebook.
The comment blames iOS (no runtime dir), but the condition switches locking off on the Mac too.
See `room_lock.rs` and docs/rooms.md "Two servers on one notebook".

## Change
1. Take room locks in embedded servers too, except on iOS: lock unless `cfg!(target_os = "ios")`.
   Prefer an explicit field on `Startup` (for example `room_locks: Option<PathBuf>` or a bool that
   defaults to on) over inferring from `stop`, so the intent is readable; ganymede-ffi sets it (off on
   iOS). Keep `Startup`'s `Default` and existing callers compiling. Fix the comment.
2. When a room is read-only because another server owns the file, the MCP tools' error and the
   execute endpoint's 400 should say so plainly and suggest the cause: another Ganymede server on this
   machine (for example the Mac app, whose Settings > Agent > MCP lets agents use its server) has the
   notebook open. Check what they say today first; change only if it doesn't already explain this.
3. Tests:
   - An embedded-style server (with `stop` set, as ganymede-ffi runs it) and a second server on the same
     notebook: whichever opens the room first owns it, the other is read-only; both orders.
   - Tests must use a temporary runtime dir. The ffi tests (crates/ganymede-ffi/tests/server.rs) must not
     lock in the real `~/Library/Jupyter/runtime`; if they would, set the runtime dir for them in a way
     that is safe with parallel tests (look at how crates/ganymede-cli/tests do it).
4. Docs: docs/rooms.md (drop "An app that embeds the server takes no lock"; say embedding apps lock
   except on iOS), the alt text and the text in docs/images/rooms/two-servers.svg, and docs/embedding.md
   if it mentions locking. Add the Unreleased note per CLAUDE.md next to the changed section.
5. Dev-log entry: docs/notes/2026-11-26-embedded-room-locks.md starting
   `## 2026-11-26, embedded servers take room locks (#966)`. Never edit docs/notes.md.

## Before committing
Run `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`, and the tests of the crates
you touched (`cargo test -p ganymede-server -p ganymede-ffi`, plus `-p ganymede-cli --test collab`).
Commit on this branch with a message ending in:

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01GitYQ6Kj6YzUy81wqQeqZR

Delete HERMES_BRIEF.md in your commit. Do not push.
