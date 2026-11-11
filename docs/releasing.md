# Releasing

How a release is cut, what it publishes, and what to do when it fails
partway through.

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

   The bump also updates the docs that name the current release: the git
   `tag` in [Embedding](embedding.md) and the sample `ganymede_version` in
   [CLI and configuration](reference.md#server-info-file). The list is
   `pre-release-replacements` in `crates/ganymede-cli/Cargo.toml`; add a
   doc there when it starts naming the release. The script also refreshes
   `e2e/wasm-smoke/Cargo.lock`, a separate workspace's lockfile that
   names each crate's version.
2. Merging that pull request with a squash merge (with CI green; branch
   protection on `main` is what guarantees this, since `release.yml`
   does not check CI itself) starts `release.yml` on the main push. A
   squash makes the release one commit, which both `release.yml` and
   `tag-release.yml` recognize. Its `version` job sees
   a workspace version that has a `CHANGELOG.md` section and no tag, so
   this run is the release: it builds the commit once, as `X.Y.Z` rather
   than a main build, and tags it `vX.Y.Z` when it publishes. It does
   not update `main-build`; the next push to `main` does.
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

Besides release commits, `release.yml` runs on every push to `main` that
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

Otherwise find the failed `release.yml` run for the release commit's
push to `main` in the Actions tab. Only the commit that changed the
version publishes; later pushes to `main` are main builds, so a merge
after the release commit can't ship under its notes. Recover from the
release workflow itself, depending on how far it got: rerun its failed
jobs, or run `tag-release.yml` by hand when the run can't be rerun.

### A job failed and nothing was published

A flaky runner, a network error, or a download that timed out: the code
is fine. The XCFramework job counts too: the release and `main-build`
wait for it as well as for the CLI targets, so a failed `xcframework`
job holds back the whole release until it is rerun. Rerun the failed
jobs from the run's page:

```sh
gh run rerun <run-id> --failed --repo tlockney/ganymede
```

If the run can't be rerun, release from a tag with `tag-release.yml`.
It tags the release commit (the last commit on `main` that set the
version), not `main`'s head, so merges since then stay out of the
release, and starts `release.yml` on the tag:

```sh
gh workflow run tag-release.yml --ref main --repo tlockney/ganymede
```

### The release commit cannot be released

A build or version check fails for a reason that needs a code change.
The tag is made only when the release publishes, so there is none yet:
fix the problem in a pull request to `main`, as usual. Its merge doesn't
change the version, so it is a main build; once it is on `main`, run
`tag-release.yml` with that merge as the commit to tag:

```sh
gh workflow run tag-release.yml --ref main -f commit=<merge sha> --repo tlockney/ganymede
```

Anything merged between the release commit and the fix ships in this
release without a line in its notes, so hold other merges until then.

If a tag was made (by `tag-release.yml`) and no GitHub Release exists
for it, nobody can have installed it, so move it: delete it with
`git push origin :refs/tags/vX.Y.Z`, fix the problem on `main`, and run
`tag-release.yml` again with the fix's merge as `commit`.

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
uploaded, so it attests the same files again, which is harmless. For a
release published from the main push, rerun only the failed jobs: a
fresh run of every job finds the tag, becomes a main build and publishes
nothing. To rebuild instead, run `tag-release.yml`, which releases from
the tag.
