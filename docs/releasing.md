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
2. Merging that pull request (with CI green; branch protection on `main`
   is what guarantees this, since `tag-release.yml` does not check CI
   itself) runs `tag-release.yml`. It sees a workspace version that has a
   `CHANGELOG.md` section and no tag, tags the merge commit `vX.Y.Z`, and
   starts `release.yml` on the tag.
3. `release.yml` builds the five targets, checks that each binary reports
   the version (x86_64 macOS under Rosetta), packages them with
   `THIRD-PARTY-NOTICES` and SHA-256 checksums, attests their build
   provenance, and publishes the GitHub Release with the changelog section
   as notes.
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

## When it fails

Find the failed `release.yml` run under the tag in the Actions tab. The
tag already exists, so pushing to `main` or rerunning `tag-release.yml`
does nothing ("vX.Y.Z exists; nothing to do"). Recover from the release
workflow itself, depending on how far it got.

### A job failed and nothing was published

A flaky runner, a network error, or a download that timed out: the code
is fine. Rerun the failed jobs from the run's page, or start a fresh run
on the tag:

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
