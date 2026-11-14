## 2026-11-14, App server info file token (ganymede-ui#583)

ganymede-ui#608 makes the Mac app write its `mcp_token`, not the server's
token, to `jpserver-<pid>.json`, so a token on disk opens only `/mcp`.
`ganymede mcp` sends the file's token only to `/mcp`, so it still works;
`jupyter server list` and other readers get a URL the server refuses.
The reference page's ganymede-ui note now says so, and `docs/mcp.md`
says its "keeps the server's token" is about `ganymede serve`'s file.
