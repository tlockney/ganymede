## 2026-10-31, Read-only `/lab/api/extensions` (#234)

The page config already named the `read-only` extension manager, but JupyterLab's Extension Manager panel still calls `GET /lab/api/extensions`, and the 404 showed as an error. The endpoint now answers as `jupyterlab`'s `ExtensionHandler` does with `ReadOnlyExtensionManager` (checked against jupyterlab 4.6.5's `extensions/manager.py`, `readonly.py`, and `handlers/extension_manager_handler.py`).

What the source showed, which the issue's reading of it missed:

- `query` absent means "list installed"; `query` present, even empty, means "search the index". The read-only manager's search returns nothing, so a search answers `[]`, not a filtered installed list.
- The handler always sends a `Link` header: the manager stores `last_page or 1`, so a manager without paging still reports page 1 as first and last. Ganymede sends path-only URLs, since it can't know the scheme behind a proxy; the panel reads only the page number.
- `ReadOnlyExtensionManager` refuses install and uninstall but inherits enable and disable, which write `page_config.json`. Ganymede refuses all four, as #234 asks, with 405 and an `ActionResult` body (`status: "error"`, a `message` naming `ganymede lab extension`) that the panel shows. JupyterLab answers a refusal with 500.
- Versions go through `get_semver_version` (`2.0.0rc1` becomes `2.0.0-rc.1`; `.devN` and `.postN` drop), so `semver_version` ports it.
- `jupyterlab_server` builds each extension's record without `author`, `license`, `bugs`, or `repository`, so JupyterLab's panel shows those as empty. Ganymede reads them from the extension's `package.json`, as the dataclass fields intend.

`enabled` uses exact names from `disabledExtensions`, as JupyterLab does, so disabling one plugin of an extension leaves the extension listed as enabled.
