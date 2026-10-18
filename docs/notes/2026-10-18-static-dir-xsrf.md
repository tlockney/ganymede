## 2026-10-18, `_xsrf` for a generic `--static-dir` (#139)

Serving a ganymede-ui build with `ganymede serve --static-dir dist` loaded
the app, but nothing it did could change state. Opening the printed
`?token=` URL swaps the token for the login cookie and redirects to `/`,
so the app is cookie-authenticated, and cookie-authenticated requests that
change state need an `_xsrf` cookie echoed in `X-XSRFToken`. Only
JupyterLab's page and the login pages set that cookie; the generic static
fallback (`ServeDir`) set none, so saves and kernel starts got 403.

The fallback is now a router with a middleware that appends the `_xsrf`
`Set-Cookie` to `text/html` responses when the browser has none, as
`jupyter_server` does on every page it renders. Assets don't get one.
`@jupyterlab/services` sends `X-XSRFToken` from the cookie on its own, so
a JupyterLab-based app needs nothing more than to connect without a token.
