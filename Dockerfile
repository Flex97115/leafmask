# syntax=docker/dockerfile:1

# ---- build stage ------------------------------------------------------------
# Compiles from source, for the host's own architecture. This is what a plain
# `docker build .` / `make docker` uses (default target: `runtime`).
FROM rust:1-bookworm AS build

# Native build deps for the full feature set:
#   cmake        -> aws-lc-sys (S3) and libssh2-sys (SSH)
#   perl, make   -> OpenSSL is vendored (statically built) for ssh2
RUN apt-get update \
    && apt-get install -y --no-install-recommends cmake pkg-config perl make \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src

COPY . .
RUN cargo build --release --features full \
    && cp target/release/leafmask /usr/local/bin/leafmask

# ---- runtime base -------------------------------------------------------
# Shared by both variants below.
FROM debian:bookworm-slim AS runtime-base

# Only ca-certificates are needed at runtime: OpenSSL is vendored into the
# binary, and MongoDB/S3/Azure TLS uses rustls/aws-lc (static).
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --create-home --uid 10001 leafmask

# ---- runtime-prebuilt: binary supplied via the build context ----------------
# Used by the release workflow: each target arch is already cross-compiled
# natively by the `binaries` job, so the multi-arch image just copies the
# matching one in instead of recompiling the whole crate — including vendored
# OpenSSL and aws-lc-sys — under QEMU emulation for arm64. Expects the build
# context to contain `bin/<amd64|arm64>/leafmask` (buildx sets TARGETARCH).
FROM runtime-base AS runtime-prebuilt
ARG TARGETARCH
COPY bin/${TARGETARCH}/leafmask /usr/local/bin/leafmask
USER leafmask
WORKDIR /home/leafmask
ENTRYPOINT ["leafmask"]
CMD ["--help"]

# ---- runtime: binary compiled in this build ----------------------------------
# The default target (last stage) — a plain `docker build .` / `make docker`
# doesn't pass --target, and shouldn't need a bin/ directory staged by hand.
FROM runtime-base AS runtime
COPY --from=build /usr/local/bin/leafmask /usr/local/bin/leafmask
USER leafmask
WORKDIR /home/leafmask
ENTRYPOINT ["leafmask"]
CMD ["--help"]
