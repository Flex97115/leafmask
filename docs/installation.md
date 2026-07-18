# Installation

Leafmask ships as a single self-contained binary. Choose whichever method suits
you — all three give you the same tool.

!!! info "Placeholder"
    Replace `OWNER` in the commands below with the GitHub owner/org once the
    repository is published.

## Install script

The quickest way. The script detects your OS and architecture, downloads the
matching prebuilt binary from the latest GitHub Release, **verifies its
checksum**, and installs it:

```sh
curl -fsSL https://raw.githubusercontent.com/OWNER/leafmask/main/install.sh | sh
```

You can tune it with environment variables:

| Variable | Default | Purpose |
| --- | --- | --- |
| `LEAFMASK_VERSION` | latest release | install a specific tag, e.g. `v0.1.0` |
| `LEAFMASK_INSTALL_DIR` | `/usr/local/bin` | where to install (falls back to `~/.local/bin`) |
| `LEAFMASK_REPO` | `OWNER/leafmask` | source repository |

Prebuilt targets: **Linux** and **macOS**, on **x86_64** and **arm64**.

```sh
# a specific version, into a user-local dir
LEAFMASK_VERSION=v0.1.0 LEAFMASK_INSTALL_DIR="$HOME/.local/bin" \
  curl -fsSL https://raw.githubusercontent.com/OWNER/leafmask/main/install.sh | sh
```

## Docker

Images are published to the GitHub Container Registry, multi-arch
(`linux/amd64`, `linux/arm64`):

```sh
docker run --rm ghcr.io/OWNER/leafmask:latest --help
```

Mount your config and a local dump directory to do real work:

```sh
docker run --rm \
  -v "$PWD/leafmask.yaml:/cfg.yaml:ro" \
  -v "$PWD/dumps:/dumps" \
  ghcr.io/OWNER/leafmask:latest --config /cfg.yaml list-dumps
```

!!! tip "Reaching MongoDB from the container"
    On Docker Desktop, a MongoDB running on your host is reachable at
    `host.docker.internal:27017`. Set `mongodb.uri` accordingly or pass
    `--uri mongodb://host.docker.internal:27017`.

## From source

Requires the [Rust toolchain](https://rustup.rs) (`rustup`, `cargo`):

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
. "$HOME/.cargo/env"
```

Leafmask uses Cargo **features** to opt into external integrations. The lean
core (transformation engine + local directory storage) needs no system
libraries; the full binary additionally needs `cmake`, `perl`, and `make`
(OpenSSL is vendored, so no system `libssl` is required).

=== "Makefile"

    ```sh
    make build-core   # core + directory storage, no external deps
    make build        # everything: MongoDB + S3 + Azure + SSH
    make install      # installs to /usr/local/bin (override with PREFIX=)
    ```

=== "Cargo"

    ```sh
    # lean core
    cargo build --release

    # everything
    cargo build --release --features full

    # pick and choose
    cargo build --release --features "mongo s3"
    ```

### Feature matrix

| Feature | Enables | Extra build deps |
| --- | --- | --- |
| *(default)* | core engine, catalog, dump management, **directory** storage | none |
| `mongo` | `dump`, `restore`, `validate --data` against MongoDB | none |
| `s3` | S3-compatible storage backend | `cmake` |
| `azure` | Azure Blob storage backend | none |
| `ssh` | SFTP storage backend | `cmake`, `perl`, `make` |
| `full` | all of the above | `cmake`, `perl`, `make` |

!!! warning "MongoDB commands need `mongo`"
    A binary built without the `mongo` feature still lists `dump`/`restore`/
    `validate` in `--help`, but invoking them prints a clear error asking you to
    rebuild with `--features mongo`. Prebuilt binaries and the Docker image
    include it.

## Verify

```sh
leafmask --version
leafmask list-transformers
```
