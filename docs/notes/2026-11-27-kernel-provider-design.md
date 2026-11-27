## 2026-11-27, kernel provider design note (#981)

#965 routes the Mac app's local notebooks through a background `ganymede
serve`, which the app's in-app TypeScript kernel can't reach through the C
ABI it registers with today. [Kernel providers](../kernel-provider.md)
proposes a websocket that carries the same calls as JSON frames, so a
connection becomes one more `AppKernel` behind `RegisteredKernels`.

It is new network surface, so it is written down before any code. The
route is narrower than the rest of the API: off by default, the server's
token only (no cookie, no tailnet login), loopback peers only, never
through a machine mount, and no kernelspec name that already exists, since
a registered kernel sees every cell run on it. A Unix socket in the
runtime directory was the close alternative; the token plus a loopback
check gives the same boundary on the listener the server already has, and
stays the fallback if the check proves hard to keep right behind proxies.
