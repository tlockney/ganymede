## 2026-11-21, Write observer: test that release may call set (#918)

`WriteObserver::set` drops the function it replaces after letting go of
its lock, so an app's `release` callback, which runs from that drop, may
call `ganymede_server_set_write_observer` itself (#917). No test held this:
dropping under the lock passed every test. A unit test now replaces a
function whose drop calls `set`, on a thread with a timeout, so a deadlock
fails the test instead of hanging it.
Another checks that the C callback is skipped for a path that isn't UTF-8
or has a NUL, as the docs say.
