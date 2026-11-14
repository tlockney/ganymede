## 2026-11-14, Tailnet identity names the person (#816)

Third step of making Ganymede Tailscale-aware. A request let in by
tailnet identity now carries the name it was let in as, and the server
uses it where it named a person before:

- `/api/me` returns the login (or a tagged device's tags) as the user's
  name and initials, so JupyterLab shows who you are instead of an
  anonymous name. A token or cookie request is anonymous as before.
- A shared notebook's room credits edits to that name (`Room::connect_as`),
  whatever name the frontend puts in its awareness state. So the `"by"`
  an agent reads can't be set by the person's own frontend.
- The request's log span gets a `who` field, and the first sign-in by
  each name is logged at info level.
- `ganymede info` shows which logins the config file lets in
  (`tailnet_identity`, null when off).

`/api/me`'s response keeps its shape; only the values change for a
tailnet login. Not yet tried on a real tailnet.
