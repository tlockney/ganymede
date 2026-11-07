## 2026-11-07, a websocket join during a rename (#509)

A collaboration client's join looked its file id's path up without the
rooms lock, then opened the room. A rename in between left it opening
the room on the old path, which no longer loads, so the client was
dropped. `send_to_notebook_room` already guarded against this with the
`moves` counter; the websocket join now goes through `join_room`, which
does the same: when a rename or delete happened since the lookup, it
looks the path up again before opening. Found in the 0.7.0 code review
(#440).
