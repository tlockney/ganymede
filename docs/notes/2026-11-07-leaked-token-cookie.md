## 2026-11-07, a leaked token outlives a restart through its cookie (#435)

security.md said a leaked token URL works until the server restarts with
a new token. Any request with the token gets a 30-day login cookie signed
with `jupyter_cookie_secret`, which persists across restarts, so one use
keeps access after the token changes. The page now says so and how to
revoke: delete the secret and restart. Tying cookies to the token that
issued them would differ from `jupyter_server`, whose cookies behave the
same way, so the docs change instead of the code.
