## 2026-11-27, write events on the events socket (#978)

The Mac app keeps answers per notebook file: a link's "always allow", the
device ledger's, folder grants. They follow the file when Ganymede writes
it, and are forgotten when something else does (#584, #916). It learned of
the server's writes through `ganymede_server_set_write_observer`, which
only reaches a server the app embeds; once a notebook goes through a
separate `ganymede serve` (#965 section 4), it needs the same reports over
the API. `/api/events/subscribe` was already there, `jupyter_events`'
stream, and emitted nothing, so the reports go there: no new route, and
JupyterLab's event manager passes an unknown schema along without acting
on it.

`WriteObserver` keeps a bounded `tokio::sync::broadcast` channel (256
events) next to the FFI function. `serve` now always builds one — the
app's when `Startup::write_observer` gives it — so a server with no embedder
still has the socket's channel, and `AppState::write_observer` hands it to
`ws::events`. A write is stamped beforehand only when someone is
listening: `is_set` is now "the FFI function is set, or the channel has a
receiver", so the cost with nobody listening is what it was.

`Stamps::observe` already had the contents-API path; `Observed` carries it
and hands it to `notify`, which sends it as the event's `path` and still
calls the FFI function with the canonical absolute path, unchanged. The
event is `jupyter_events`' envelope — `__schema__`
(`https://ganymede-server.com/events/write/v1`), `__schema_version__` and
`__metadata_version__` 1, `__timestamp__` — over `path`, `before` and
`after`. `jiff` writes the timestamp: the server's own time crate, so no
chrono and no new dependency.

A subscriber that lags has missed writes and can't tell which, so its
socket is closed with 1011 and the reason `missed events`; the app forgets
the answers it can't confirm, which fails safe. That is the one close a
server sends unasked on this route, and a client that reads it knows to
re-read the files it depends on.

The tests drive it through the socket: a save made and remade through the
contents API, a copy to a named path and a checkpoint restore each send
one event with the right stamps, while a failed write, a directory save, a
new untitled file, a rename and a delete send none; and a room's autosave
and a run's save of its outputs send theirs. The lag is a unit test of the
frame the socket sends next (`ws::next_event_frame`), on a channel of two:
a subscriber that read nothing while five writes went by gets the 1011
close, and one that read each gets all five. Driving it through a real
socket first depended on the OS's buffers filling, which macOS's did and
Linux's, several megabytes on loopback, did not. And a send to a client
that stopped reading could wait forever, never polling the channel, so it
never saw the lag; each send now has 10 seconds, after which the client
is dropped and its subscription with it.

A write that begins with no app function set is still not reported to one
set before it ends, as before: `Observed` notes whether one was set when
it took the stamp, since a socket's subscriber now makes that stamp be
taken anyway. The event's timestamp is taken once per write, in `notify`,
so every subscriber gets the same one. A test in `tests/embedded.rs`
drives a server built by `run_with`, with and without the app's
observer, to pin the wiring. A unit test pins the
rest: nothing is stamped before a write with neither a function set nor a
subscriber.
