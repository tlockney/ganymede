## 2026-10-18, limits on `lab install` downloads (#47)

`lab install` and `lab extension install` fetched whatever URL PyPI's JSON
named, following redirects, with no timeouts and no size cap, and then
unpacked the wheel with no cap either. They ignored `yanked`, and they
dropped every version specifier, so a dependency always got its latest
release.

What changed, all in a new `lab/pypi.rs`:

- The agent is HTTPS only, with connect, response, body and overall
  timeouts. The API must answer from `pypi.org` and wheels from
  `files.pythonhosted.org`, checked on the final URL after redirects.
  `GANYMEDE_PYPI_URL` names a mirror (its host is allowed too), and
  `GANYMEDE_PYPI_HOSTS` adds more hosts.
- Downloads are capped at 256 MB. Extraction is capped at 100,000
  entries, 256 MB per file and 1 GB in all (`Limits`).
- Yanked files and releases are skipped. An explicit `==` pin or
  `lab install --version` on a yanked release fails and gives the reason.
- PEP 440 versions and specifiers (`==`, `!=`, `<`, `<=`, `>`, `>=`, `~=`,
  `==X.*`) apply on the command line and in `Requires-Dist`. An installed
  dependency is upgraded when it no longer matches. Pre-releases are picked
  only when asked for or when nothing else matches, with a note.
- `--no-deps`, the wheel's URL and SHA-256 printed before install, and a
  JupyterLab reinstall that renames the old copy aside and restores it if
  the move fails.
- [frontends.md](../frontends.md#what-a-download-trusts) says what this
  trusts: the hash comes from the same response as the URL, so it guards
  against corruption, not a malicious release.

Not done: attestation or RECORD checks, a lock file, `--from-wheel`,
extension-to-JupyterLab compatibility checks, and locking against
concurrent installs.
