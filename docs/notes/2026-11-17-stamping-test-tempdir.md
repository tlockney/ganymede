## 2026-11-17, flaky stamping test: the root was deleted under a waiting task (#849)

`stamping::tests::indexing_the_new_path_waits_for_the_create` failed once
in CI with `NotFound("sub/Untitled.ipynb")` right after the create had
made that file. The server code was right; the test deleted the file.

`HeldCreate::finish` took `self`, so its temporary root (`TempDir`) was
dropped, and the directory deleted, when `finish` returned. The index
task the test had spawned was waiting on the create's claim on `sub`;
once the create ended it went on to `contents.get`, which `FsContents`
runs on a blocking thread. The index task's `get` usually finished
before `finish` dropped the root; when it didn't, the file was gone.

A 100 ms delay in the test contents' `get` makes the test fail on every
run on main (as `OutsideRoot`, since by then the root itself can't be
resolved; same cause), and not with the fix. `finish` now returns the root
and is `#[must_use]`, so each caller keeps it until the task it spawned
has ended.

Lesson: in a test, a helper that consumes the fixture must not end the
fixture's lifetime while tasks spawned against it are still running;
hand the guard back to the caller.
