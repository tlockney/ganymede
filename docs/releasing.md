# Releasing

How a release is cut, what it publishes, and what to do when it fails
partway through.

## Order across the repositories

A round releases cdp-kernel, then Ganymede, then ganymede-ui. Each
release picks up the ones before it in its own release commit, so no
separate pull request moves a pin:

- Ganymede's `scripts/release.sh` runs `scripts/release-pins.sh`, which
  moves CI's and the container image's cdp-kernel to npm's latest
  release, and `ganymede ui install`'s default to ganymede-ui's latest
  release, with the SHA-256 that release's `SHA256SUMS` lists and its
  zip matches.
- ganymede-ui's release script moves its cdp-kernel and its
  `GANYMEDE_REF` to the latest releases, so the apps link the
  XCFramework this release published.

`ganymede ui install` therefore installs the ganymede-ui release from
before the round until the next Ganymede release; `--version` installs
a newer one, checked against its release's `SHA256SUMS`. Release
cdp-kernel first only when it has changes; the scripts take whatever is
latest.

## The normal path

1. From an up-to-date `main`, `scripts/release.sh <level>` previews the
   version bump and the changelog. With `--execute` it commits them on
   `release/vX.Y.Z`, pushes, and opens an issue and a pull request.
   Before merging it, remove the docs' "Unreleased" notes on that branch
   ([Development](development.md#this-site)): the release ships everything
   they describe. Reword the text around a note if it read as unreleased;
   a note added to `main` after the branch was cut stays. This finds them, and the
   script lists them in the pull request:

   ```sh
   grep -rn 'Unreleased\.' README.md docs/*.md site/index.html
   ```

   Notes that say "Unreleased in ganymede-ui." stay: they describe the
   apps, which ship with ganymede-ui's release, and its release script
   opens an issue here to update them then.

   The commit also moves the cdp-kernel and ganymede-ui pins to their
   latest releases (above); the script prints each, and the commit
   message and pull request list them. Run `scripts/release-pins.sh
   --check` to see them without releasing.

   The bump also updates the docs that name the current release: the git
   `tag` in [Embedding](embedding.md) and the sample `ganymede_version` in
   [CLI and configuration](reference.md#server-info-file). The list is
   `pre-release-replacements` in `crates/ganymede-cli/Cargo.toml`; add a
   doc there when it starts naming the release. The script also refreshes
   `e2e/wasm-smoke/Cargo.lock`, a separate workspace's lockfile that
   names each crate's version.
2. Merging that pull request (with CI green; branch protection on `main`
   is what guarantees this, since `tag-release.yml` does not check CI
   itself) runs `tag-release.yml`. It sees a workspace version that has a
   `CHANGELOG.md` section and no tag, tags the merge commit `vX.Y.Z`, and
   starts `release.yml` on the tag.
3. `release.yml` builds the five targets, checks that each binary reports
   the version (x86_64 macOS under Rosetta), packages them with
   `THIRD-PARTY-NOTICES` and SHA-256 checksums, attests their build
   provenance, and publishes the GitHub Release with the changelog section
   as notes. The release also carries `GanymedeFFI.xcframework.zip`, the
   library the Mac and iPad app links, with
   `GanymedeFFI.xcframework.zip.sha256` beside it, under the same name
   every release, so ganymede-ui downloads it for the tag it pins instead
   of building it ([Embedding](embedding.md#from-swift)).
   It then redeploys the site from `main`, and builds the container image
   ([`container/Dockerfile`](../container/Dockerfile)) from the release,
   smoke-tests it, and pushes it to `ghcr.io/tlockney/ganymede` as the
   version and `latest`, for amd64 and arm64. The first time the package
   is pushed, GitHub creates it as private: make it public in the
   package's settings.

Then check the result:

```sh
gh release view vX.Y.Z --repo tlockney/ganymede
gh release download vX.Y.Z --repo tlockney/ganymede -p '*x86_64-unknown-linux-gnu.tar.gz'
gh attestation verify ganymede-X.Y.Z-x86_64-unknown-linux-gnu.tar.gz --repo tlockney/ganymede
curl -fsSL https://ganymede-server.com/install.sh | sh -s -- --version X.Y.Z --no-kernel
docker run --rm ghcr.io/tlockney/ganymede:X.Y.Z ganymede --version
```

The landing page's Mac download button finds the newest
`Ganymede-macOS-X.Y.Z.zip` in ganymede-ui's latest release by itself.
After a ganymede-ui release that changes how the apps look, re-shoot the
app screenshots ([Development](development.md#app-screenshots)).

## Main builds

Besides releases, `release.yml` runs on every push to `main` that
changes more than docs, and on a manual run on `main`. It builds the
same five targets with `GANYMEDE_VERSION_BUILD=main.<commit>`, so
`ganymede --version` prints `X.Y.Z+main.<commit>`, packages them as
`ganymede-main-<target>` (and the XCFramework under its usual name),
attests them, and replaces the assets of the `main-build` prerelease,
which `install.sh --main` installs. A newer push cancels the older
run's builds, but not its upload, which finishes first. Being a
prerelease, `main-build` is never the "latest" release that the plain
install fetches. The job moves the `main-build` tag to the commit when
GitHub lets the workflow token do so; when it can't (a ref update
across commits that change workflow files), the tag lags and the run
warns, but the assets, notes, and attestation still name the right
commit. A failed main build changes nothing; the next push or a manual
run on `main` replaces it.

## When it fails

If `scripts/release.sh --execute` stops after its commit (the push, the
issue or the pull request failed), run it again from the
`release/vX.Y.Z` branch it left you on, with `--execute`: it skips the
bump, pushes the branch, and opens whichever of the issue and pull
request doesn't exist yet. If it stops earlier, the branch doesn't
exist and a rerun from `main` starts over.

If `tag-release.yml` pushed the tag but failed to start `release.yml`,
rerun it: with the tag on the merge commit and no release run for it,
it starts the release.

Otherwise find the failed `release.yml` run under the tag in the
Actions tab. Once it has run, pushing to `main` or rerunning
`tag-release.yml` does nothing ("vX.Y.Z exists and release.yml has run
for it"). Recover from the release workflow itself, depending on how
far it got.

### A job failed and nothing was published

A flaky runner, a network error, or a download that timed out: the code
is fine. The XCFramework job counts too: the release and `main-build`
wait for it as well as for the CLI targets, so a failed `xcframework`
job holds back the whole release until it is rerun. Rerun the failed
jobs from the run's page, or start a fresh run on the tag:

```sh
gh run rerun <run-id> --failed --repo tlockney/ganymede
# or
gh workflow run release.yml --ref vX.Y.Z --repo tlockney/ganymede
```

### The tagged commit cannot be released

A build or version check fails for a reason that needs a code change. If
no GitHub Release exists for the tag, nobody can have installed it, so
move the tag:

1. Delete the tag: `git push origin :refs/tags/vX.Y.Z`.
2. Fix the problem in a pull request to `main`, as usual.
3. Retag the new head of `main` and start the release:
   `gh workflow run tag-release.yml --ref main --repo tlockney/ganymede`.

The changelog section stays as `scripts/release.sh` wrote it; the fix
appears in the next release's notes.

### The release was published, but is wrong or incomplete

Once a release is public, someone may have installed it, and
`install.sh` installs the latest by default. Don't replace its assets or
move its tag. Fix the problem on `main` and cut a patch release with
`scripts/release.sh patch`. If the broken release must not be installed,
mark it a prerelease, so `releases/latest` (what `install.sh` picks) moves
back to the one before:

```sh
gh release edit vX.Y.Z --prerelease --repo tlockney/ganymede
```

### Publishing failed after the release was created

If the `release` job created the GitHub Release and then failed to upload
part of it, a rerun fails because the release exists. Within minutes of
the failure, while it is plainly incomplete, delete the release but keep
the tag, then rerun the job:

```sh
gh release delete vX.Y.Z --yes --repo tlockney/ganymede   # no --cleanup-tag
gh run rerun <run-id> --failed --repo tlockney/ganymede
```

A rerun of only the failed job reuses the archives the build jobs already
uploaded, so it attests the same files again, which is harmless. A fresh
run rebuilds them, and the new archives get their own attestations.
