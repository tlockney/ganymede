## 2026-11-11, The API by example: review follow-ups (#689)

Lows from the review of #707, fixed after it merged:

- The intro said every exchange came from ipykernel. The run example
  came from cdp-kernel, whose package-stats notebook is in cdp-kernel
  #472. The intro now says so.
- The new examples sent `Authorization: token <token>`. They now send
  `tok`, the token the page's server uses.
- The grant section named `jupyter.host()`, which is cdp-kernel's client.
  A grant covers host calls from any kernel, so the section now says
  "the host API". It is also marked as Ganymede's own route, like the run
  section.
- The grant section now gives the `404` and `409` answers. "Every route"
  links the sections on this page as well as reference.md.
