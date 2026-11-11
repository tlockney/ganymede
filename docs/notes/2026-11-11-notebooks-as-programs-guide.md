## 2026-11-11, "Notebooks as programs" guide (#689)

Running a notebook with parameters and named results is the project's
first priority, but its docs were spread over five places: a subsection
of Getting started, the formats and grants in reference.md, a route
name in api.md, the host API design note in Background, and the links
and Shortcuts tables in ganymede-ui's app guide. The host API was
explained only in the design note.

docs/programs.md walks one notebook (`fremont.ipynb`, parameters
`counter` and `days`, result `weekday_drop`, as in Getting started)
through every caller: `ganymede execute -p --results`, CI,
`POST /api/notebooks/run`, `ganymede://run` links and the Run Notebook
with Parameters Shortcut, `host.notebooks.run` from another notebook,
and grants. It links to the reference for formats instead of repeating
them. The sidebar gets a "Notebooks as programs" group after "Use the
apps".

Found on the way: cdp-kernel's reference still says
`host.notebooks.results`, `run` and `self.*` are "not on a server yet",
though v0.8.0 serves them; that is fixed in cdp-kernel (#468).
