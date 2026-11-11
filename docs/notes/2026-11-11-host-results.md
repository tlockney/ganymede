## 2026-11-11, host API: a notebook reads another's named results (#660)

`notebooks.results { path }` returns `{ name: value }` from the
notebook's outputs, using the run contract's reader
(`ganymede_nbformat::named_results`, #651), so the host API, `ganymede
execute --results` and later `notebooks.run` agree on what a result is.
It reads the copy `notebooks.read` returns: the room when one is open,
so results a client hasn't saved yet count, and disk otherwise. It sits
under the `[host] read` grant, like the other reads: a notebook that may
read another may read its results.
