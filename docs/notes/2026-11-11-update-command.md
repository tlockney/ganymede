## 2026-11-11, `ganymede update` (#701)

Thomas asked for an `update` command that does in place what install.sh
does. It is Rust in ganymede-cli, not a call to install.sh, so it works
on Windows too and needs no shell, curl or tar. It keeps install.sh's
checks: the latest version from the `/releases/latest` redirect, the
archive and its `.sha256` over HTTPS (redirects only to GitHub's asset
hosts, through the `Releases` type `ganymede ui install` already used),
the SHA-256 checked before unpacking, the native arm64 build under
Rosetta, and the new binary run with `--version` before it replaces
anything. That last run also stands in for install.sh's glibc check: a
binary that can't load fails there and the old one stays. A release must
report exactly `ganymede X.Y.Z`.

The target comes from the running binary's build (`GANYMEDE_BUILD_TARGET`),
so a source build for a target releases don't cover (musl, say) is told
to update from source. Binaries from `cargo install`, Homebrew, Nix or a
cargo `target` directory, and debug builds, are refused with how to
update them instead; overwriting them would leave the package manager's
record wrong.

Choices made conservatively, open to change: it updates only the
`ganymede` binary. cdp-kernel (npm), JupyterLab and ganymede-ui each have
their own install commands, and it prints the ones to run for what is
installed rather than running npm or more downloads itself. A main build
(`+main.` in its version) follows main by default; `--version` goes back
to a release. Only newer releases install unless `--version` names one,
which may be older.

The latest version is always asked of GitHub, as install.sh does;
`GANYMEDE_DOWNLOAD_BASE` only moves the file downloads to a mirror.

On Windows a running `.exe` can't be overwritten but can be renamed, so
the old binary moves to `ganymede.exe.<pid>.old` and a later update
removes it. The pid keeps a server still running from an earlier update's
`.old` file from blocking the next one. After a release, main-build lags
until the next push to main, so `--main` can install a build whose base
version is older than the release; it says so when that happens. That path is untested, like the rest of Windows (best effort).

New dependencies: `tar` (no default features, so no xattr) to read the
release's `.tar.gz`, and `flate2` declared directly; it was already in the
tree through `zip` with the same zlib-rs backend. `save_checked`'s
mismatch message said "PyPI lists" for every download; it now says
"expected".
