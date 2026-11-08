## 2026-11-08, container base images pinned by digest (#549)

`container/Dockerfile` took `node`, `debian` and `rust` by tag, so a
rebuild could get a different image under the same tag, and nothing
proposed updates. Each `FROM` now names its tag and the multi-arch index
digest (from the registry's `Docker-Content-Digest` for the OCI index),
and `dependabot.yml` has a weekly grouped `docker` entry for `/container`.
The `NODE_VERSION` and `RUST_VERSION` args went: a digest overrides the
tag, so the args would have changed nothing, and CI never passed them.
