## 2026-11-19, `ganymede update` leaves an app bundle's binary alone (#883)

The Mac app is to ship the `ganymede` CLI inside `Ganymede.app` and link
it into PATH from Settings (ganymede-ui#686). `ganymede update` renames
a new binary over its own canonical path, and for the app's copy that
path is inside the signed bundle: the write would break the app's code
signature.

- `managed()` now treats a path with a `*.app` component followed by
  `Contents` (case-insensitive) as the app's, and `update` and
  `update --check` stop with "this ganymede comes with the Ganymede app;
  it updates with the app". A link to it resolves there too, since the
  path is canonicalized first.
- A bare `.app` directory name, or a `*.app` not followed by `Contents`,
  is not matched.
- `docs/install.md` names app bundles among the binaries `update`
  refuses.

The app bundles the CLI only from a release that has this.
