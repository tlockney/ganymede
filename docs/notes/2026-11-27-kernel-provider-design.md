## 2026-11-27, kernel provider design note (#981)

#965 routes the Mac app's local notebooks through a background `ganymede
serve`, which the app's in-app TypeScript kernel can't reach through the C
ABI it registers with today. [Kernel providers](../kernel-provider.md)
proposes a channel that carries the same calls as JSON frames, so a
connection becomes one more `AppKernel` behind `RegisteredKernels`.

It is new surface, so it is written down before any code. The first draft
was a websocket route guarded by the token and a loopback check. The
pre-push review showed why that is weak: `tailscale serve` sends no
identity for tagged devices or funnel traffic, `ssh -L` and local proxies
arrive from loopback too, and the token reaches a browser through
`?token=`. The note now uses a Unix socket in the runtime directory, whose
file modes and a peer-user check make "this user's processes" the
boundary, with the token as a second check.

The review also found that the code didn't do what the first draft said
after a disconnect: a lost registered kernel is restarted, and
`RegisteredKernels::launch` falls back to the subprocess provisioner for a
name no longer registered, which would run a provider's `argv`. The note
now drops `argv` and `env` from a provider's spec, never launches a lost
socket name through the fallback, and ends such kernels with no restart,
and lists those changes, the start acknowledgement `AppKernel::start`
can't give today, and wiring `RegisteredKernels` into `ganymede serve`.

Also here: a wording fix in docs/api.md's write events ("another client
of this route", from #980's review).
