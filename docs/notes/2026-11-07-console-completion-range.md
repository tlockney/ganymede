## 2026-11-07, console completions replace cursor_start..cursor_end (#478)

`ganymede console` handed rustyline the kernel's `cursor_start` as is, so
one past the cursor panicked on a reversed range, and it ignored
`cursor_end`, so completing inside a word left the word's tail behind.
`completion_range` turns both code-point offsets into byte offsets, with
the start at most the cursor and the end at least the cursor and at most
the line. rustyline's `Completer::update` replaces only up to the cursor,
so `KernelHelper` overrides it to replace up to the stored end. Found in
the 0.7.0 code review (#440).
