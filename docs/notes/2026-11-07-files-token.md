## 2026-11-07, `?token=` on `/files/` (#508)

A page URL with a correct `?token=` sets the login cookie and redirects
to the same URL without the token, so the token leaves the address bar.
`/files/` went through that redirect too, which broke a download by a
client without a cookie jar: `curl 'http://…/files/data.csv?token=…'`
followed the 302 and got the login page's 403. `/files/` now keeps the
token like the API paths do, still gaining the cookie. `jupyter_server`
accepts the query token on its files handler the same way. Found in the
0.7.0 code review (#440).
