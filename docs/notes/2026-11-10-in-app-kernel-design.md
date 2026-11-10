## 2026-11-10, in-app kernel registration design (#266)

On the iPad and iPhone the in-app kernel runs beside the embedded server,
not through it: the notebook page runs its own kernel service and Swift
relays messages to a Worker in a hidden web view. So rooms, the execute
endpoint and the agent tools built on it don't reach notebooks on that
kernel. The direction review of 2026-11-09 put fixing that before the
choice of a native engine, and this note is the design for review before
any code: [In-app kernels as server kernels](../in-app-kernel.md).

It proposes three C functions and a callback table that register the
app's kernel with the embedded server, with Jupyter JSON per channel,
and keeps today's Worker kernel unchanged behind them. It also covers a
"lost" reason for kernels iOS ends in the background with a Restore
action, kernel capabilities in kernelspec metadata, and the two places a
kernel core could run once a native engine replaces the Worker. Reading
the code for it showed that the kernel host is per window today
(`WebView.swift` makes one per web view), so registration moves it to
the app, and that cdp-kernel's browser kernel doesn't use the
service-worker relay, which only matters once Pyodide registers.
