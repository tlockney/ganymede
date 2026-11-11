## 2026-11-11, The API by example: runs and grants (#689)

api.md showed requests and answers for the Jupyter routes but only
listed Ganymede's own `POST /api/notebooks/run` and
`/api/host/grants/{path}`; their shapes were in reference.md's prose. Two
sections now show each with a real request and answer, captured from
`ganymede serve` 0.9.0 running cdp-kernel's package-stats example: a run
that succeeds, one that fails after its kernel started (`200`, `"status":
"error"`), a notebook that can't be read (`404`), and a grant's `PUT`
with its `defaults`. reference.md stays the place for every key and
status.
