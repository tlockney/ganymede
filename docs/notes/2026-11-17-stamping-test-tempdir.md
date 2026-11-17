## 2026-11-17, flaky stamping test: the root was deleted under a waiting task (#849)

`stamping::tests::indexing_the_new_path_waits_for_the_create` failed once
in CI with `NotFound("sub/Untitled.ipynb")` right after the create had
made that file. The server code was right; the test deleted the file.

`HeldCreate::finish` took `self`, so its temporary root (`TempDir`) was
dropped, and the directory deleted, when `finish` returned. The index
task the test had spawned was waiting on the create's claim on `sub`;
once the create ended it went on to `contents.get`, which `FsContents`
runs on a blocking thread. On a current-thread runtime the test task
usually got there first only by luck of scheduling: when the index task
was polled after `finish` returned, the file was gone.

A 100 ms delay in the test contents' `get` makes the failure happen on
every run on main, and not with the fix. `finish` now returns the root
and is `#[must_use]`, so each caller keeps it until the task it spawned
has ended.

Lesson: in a test, a helper that consumes the fixture must not end the
fixture's lifetime while tasks spawned against it are still running;
hand the guard back to the caller.
