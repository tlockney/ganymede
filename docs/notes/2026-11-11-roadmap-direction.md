## 2026-11-11, roadmap follows the four priorities (#666)

Thomas set Ganymede's direction as four priorities in order: the
notebook as a program, the apps as a full runtime, a person and an
agent in one live notebook, and TypeScript notebooks for TypeScript
developers. The roadmap still had Pyodide as the second "Next" item,
kept the native in-app kernel and the host API under "Open" beside
CurveZMQ, and didn't mention parameters, named results or the run
endpoint.

"Next" is now those four priorities, each with its open issues. Work
merged since v0.7.0 (run contract, the first host API calls, kernel
registration through the C ABI) sits in a "Done on `main`" list under
one Unreleased note, so the release moves it into "Done". A new "Not
now" section holds what is wanted eventually but deferred: Pyodide,
MCP Apps, a native non-JS kernel and CurveZMQ. "Open" keeps the
undecided items.
