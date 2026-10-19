## 2026-10-19, API examples and switching notes (#183, #187, #188)

- The API examples were captured from `ganymede serve` with ipykernel,
  not written by hand. Two things that came out of it: `default` in
  `/api/kernelspecs` is the first kernelspec found when
  `--default-kernel` isn't set, so Known gaps and Replacing Python were
  wrong to say there is no default; and the `409` on `last_modified`
  and `412` on `If-Match` are both Ganymede's own checks, which
  `jupyter_server` doesn't make, so the page says so.
- The complete `ganymede.toml` on the reference page was checked by
  starting `ganymede serve --config` with it.
- cdp-kernel's `kernel.json` on the Install page is the one
  `cdp-kernel install --runtime node` writes (0.7.0), with the machine's
  paths replaced by typical ones.
