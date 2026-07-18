# syntax=docker/dockerfile:1

# ---- build stage ----------------------------------------------------------
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

# ---- runtime stage --------------------------------------------------------
FROM debian:bookworm-slim AS runtime

# Only ca-certificates are needed at runtime: OpenSSL is vendored into the
# binary, and MongoDB/S3/Azure TLS uses rustls/aws-lc (static).
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --create-home --uid 10001 leafmask

COPY --from=build /usr/local/bin/leafmask /usr/local/bin/leafmask

USER leafmask
WORKDIR /home/leafmask
ENTRYPOINT ["leafmask"]
CMD ["--help"]
