## 2026-10-19, CORS credentials off by default, `allow_credentials` to turn them on (#170)

The CORS layer for `allow_origins` never sent `Access-Control-Allow-Credentials`, so a page on a listed origin could not use the login cookie, while `docs/security.md` warned that `'*'` plus the cookie let any page act with your session. `jupyter_server` behaves the same by default: its `allow_credentials` is `False`. So the behaviour stays, the docs now say cross-origin clients send the token, and `allow_credentials` (`--allow-credentials`) is added under `jupyter_server`'s name.

With `*` listed, credentials stay off. `jupyter_server` answers `Access-Control-Allow-Origin: *` there, which browsers never pair with credentials. Ganymede echoes the request's origin instead, so sending credentials would hand the cookie session to every page.

Lesson: the cookie is `SameSite=Lax`, so even with credentials a browser sends it only between origins on the same site. CORS is one gate and the cookie's own attributes are another, and the docs need to describe both.
