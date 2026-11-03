## 2026-11-03, live outputs cost what they add (#286)

Server-side execution (#244) sent the room a cell's whole output list
for every iopub message: the worker serialized every output, and
`schema::set_outputs` re-encoded the room's outputs to compare them and
ran `strip_prefix` over the whole stream. A print loop cost O(n²) in its
output size on the room task.

Now the worker tracks how many of the running cell's outputs the room
has (`LiveOutputs` in execute.rs) and sends only the new ones as
`RoomCmd::Append`. `schema::append_outputs` merges a stream into the
stream before it by reading only that output's `output_type` and `name`,
then appends to its `YText`. A clear or a display with an id still
resends the whole list, as before. A `clear_output(wait)` changes no
output when it arrives, so ganymede-exec reports no changed cell for it;
the handler has to note every clear itself, or the next output (one
output replacing one) looks like nothing new and the live view freezes
until the cell ends. The first update of each run also resends, since
the room may still show outputs from before it.

yrs has its own quadratic term: `Text::insert` walks the text's blocks
from the start, and each insert in its own transaction adds a block, so
N single-line appends cost O(N²) in yrs (Yjs avoids this with search
markers). So the room loop also merges `Append`s already queued for the
same cell into one edit. When the room falls behind a fast loop, it
makes fewer, larger edits.

Measured with the fake kernel's new `lines <n>` cell (one stream message
per line), release build: 20,000 lines finished in 1.6 s. On main, the
same run had not finished after 60 s.
