# Agent journals

Two analyses Claude Code kept as notebooks with the
[ganymede-journal plugin](../../docs/mcp.md#the-ganymede-journal-plugin-for-claude-code),
in TypeScript on cdp-kernel's Node kernel:

- [fremont/](fremont/): have bike commutes over Seattle's Fremont Bridge
  come back since 2019?
- [oklahoma/](oklahoma/): did Oklahoma's earthquakes stop?

Each folder has the notebook and the module it imports in `journal/`,
`prompts.md` with what the person asked, and `fetch-data.sh`, which
downloads the data the notebook reads into `data/` (not committed).

The notebooks were kept on 2026-11-09 and re-run since on Ganymede 0.9.0
and cdp-kernel 0.10.0 with freshly downloaded data. To run them again:

```sh
examples/journals/run.sh
```

The site renders them, with their prompts, under
[Examples](https://ganymede-server.com/docs/examples).
