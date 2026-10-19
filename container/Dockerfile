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
# checkout instead (CI on pull requests).
#
# NODE_VERSION and CDP_KERNEL_VERSION are what CI tests (ci.yml).

ARG GANYMEDE_SOURCE=release
ARG RUST_VERSION=1.95
ARG NODE_VERSION=24
ARG CDP_KERNEL_VERSION=0.7.1

FROM node:${NODE_VERSION}-trixie-slim AS node

# The binary, from the release or from the checkout. Only the stage that
# GANYMEDE_SOURCE names is built.
FROM debian:trixie-slim AS binary-release
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

FROM rust:${RUST_VERSION}-trixie AS binary-checkout
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p ganymede-cli \
 && mkdir /out && cp target/release/ganymede /out/

FROM binary-${GANYMEDE_SOURCE} AS binary

FROM debian:trixie-slim
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
