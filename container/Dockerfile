# syntax=docker/dockerfile:1
#
# Ganymede serving JupyterLab, with cdp-kernel for TypeScript notebooks,
# and no Python in the image (#203). Build from the repository root:
#
#   docker build -f container/Dockerfile -t ganymede .
#   docker run --rm -p 8888:8888 ganymede
#
# By default the binary is the GitHub release whose version is in the
# checkout's Cargo.toml, so an image built from a release tag matches
# what's on GitHub. --build-arg GANYMEDE_SOURCE=checkout builds the
# checkout instead, and GANYMEDE_SOURCE=local copies a Linux binary
# already built at container/bin/ganymede (CI, which builds it once for
# several jobs, #460).
#
# CDP_KERNEL_VERSION is what CI tests (ci.yml). The base images are
# pinned by tag and multi-arch digest; Dependabot updates both. The Rust
# image's tag matches ci.yml's linux-release container, and Node's major
# version what CI tests.

ARG GANYMEDE_SOURCE=release
ARG CDP_KERNEL_VERSION=0.11.0

FROM node:24-trixie-slim@sha256:0820eaa85e07d0490446dd01d14707e17e6108a36988cf374899cd057eb3812d AS node

# The binary, from the release or from the checkout. Only the stage that
# GANYMEDE_SOURCE names is built.
FROM debian:trixie-slim@sha256:a72b4433de1cf7d6bae0c4cee74d0547f58601a643b3375f070c51600d63b6b2 AS binary-release
ARG TARGETARCH
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl \
 && rm -rf /var/lib/apt/lists/*
COPY Cargo.toml /tmp/
RUN set -e; \
    case "$TARGETARCH" in \
      amd64) target=x86_64-unknown-linux-gnu ;; \
      arm64) target=aarch64-unknown-linux-gnu ;; \
      *) echo "no ganymede release for $TARGETARCH" >&2; exit 1 ;; \
    esac; \
    version=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' /tmp/Cargo.toml); \
    name="ganymede-$version-$target"; \
    base="https://github.com/tlockney/ganymede/releases/download/v$version"; \
    cd /tmp; \
    curl -fsSLO "$base/$name.tar.gz"; \
    curl -fsSLO "$base/$name.tar.gz.sha256"; \
    sha256sum -c "$name.tar.gz.sha256"; \
    tar -xzf "$name.tar.gz"; \
    mkdir /out; \
    mv "$name/ganymede" /out/

FROM rust:1.95-trixie@sha256:f49565f188ee00bc2a18dd418183f2c5f23ef7d6e691890517ed341a598f67c3 AS binary-checkout
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p ganymede-cli \
 && mkdir /out && cp target/release/ganymede /out/

FROM debian:trixie-slim@sha256:a72b4433de1cf7d6bae0c4cee74d0547f58601a643b3375f070c51600d63b6b2 AS binary-local
COPY container/bin/ganymede /out/ganymede

FROM binary-${GANYMEDE_SOURCE} AS binary

FROM debian:trixie-slim@sha256:a72b4433de1cf7d6bae0c4cee74d0547f58601a643b3375f070c51600d63b6b2
ARG CDP_KERNEL_VERSION

# ca-certificates: `ganymede lab install` and other HTTPS downloads use
# the system trust store.
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*

COPY --from=node /usr/local/bin/node /usr/local/bin/node
COPY --from=node /usr/local/lib/node_modules/npm /usr/local/lib/node_modules/npm
RUN ln -s ../lib/node_modules/npm/bin/npm-cli.js /usr/local/bin/npm \
 && ln -s ../lib/node_modules/npm/bin/npx-cli.js /usr/local/bin/npx
COPY --from=binary /out/ganymede /usr/local/bin/ganymede

# Kernelspecs and JupyterLab's files go in system directories rather than
# the home directory, so a volume mounted over it doesn't hide them.
ENV GANYMEDE_DATA_DIR=/usr/local/share/ganymede
RUN npm_config_cache=/tmp/npm-cache npm_config_update_notifier=false \
    npm install -g --no-audit --no-fund "@tlockney/cdp-kernel@${CDP_KERNEL_VERSION}" \
 && rm -rf /tmp/npm-cache \
 && JUPYTER_DATA_DIR=/usr/local/share/jupyter cdp-kernel install --runtime node \
 && ganymede lab install

RUN useradd --create-home --uid 1000 ganymede \
 && mkdir /notebooks && chown ganymede:ganymede /notebooks
USER ganymede
WORKDIR /notebooks
EXPOSE 8888
CMD ["ganymede", "lab", "--no-browser", "--ip", "0.0.0.0", "--port", "8888"]
