## 2026-11-13, the device reference comes from ganymede-ui (#749)

ganymede-device moved into ganymede-ui as `packages/device`
(tlockney/ganymede-ui#560), so `npm run fetch-cell-api` copies the
device reference from `packages/device/docs/reference.md` on
ganymede-ui's main. Each source now names its docs directory, which the
link rewriting strips for renames and adds back for GitHub links. A
local checkout is `GANYMEDE_UI_DIR`, no longer `GANYMEDE_DEVICE_DIR`.

Review follow-ups: the cell API device page's "Edit this page" link
points at ganymede-ui's `packages/device/src/device.ts`; `relink` throws
if a `files` key is outside `source.docs`, and a test covers the rename
for a docs directory other than `docs/`.
