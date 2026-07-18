# Leafmask

A stateless CLI tool for logical **MongoDB** database dumping, deterministic data
anonymization, synthetic data generation, referentially-intact subsetting, and
restoration.

Leafmask has no internal user/auth system of its own — access control is
delegated to the MongoDB and storage-backend credentials supplied to the CLI.

This repository is a **Rust regeneration** of the product described in
[`.spectial/`](.spectial/), which is the source of truth for the feature set,
dependency graph, and acceptance criteria.

## What it does

Storage-only commands (no MongoDB needed — work in any build):

- **`list-transformers`** — list every available transformer (built-in and any
  custom ones declared in the config), each with a short description.
- **`show-transformer <name>`** — show one transformer's full documentation:
  parameters, their types, and whether they are required.
- **`list-dumps`** — list all dumps in the configured storage with id and status.
- **`show-dump <id|latest>`** — show a dump's table of contents and metadata.
- **`delete`** — delete a dump by id, or prune by a retention policy
  (`--retain-recent`, `--before-date`, `--prune-failed`, `--prune-unsafe`), with
  `--dry-run`.

MongoDB commands (built with `--features mongo`; a clear error otherwise):

- **`dump`** — logical dump with db/collection filters, inline BSON binary,
  index/option capture, optional `--gzip`, transformations applied while
  streaming.
- **`restore <id|latest>`** — restore a dump into MongoDB with filters,
  `--dependency-order`, batching, error exclusions, and pre/post scripts.
- **`validate --data`** — before/after diff of the configured transformations
  over a sample (`--format text|json`, `--table-format vertical|horizontal`,
  `--transformed-only`, `--strict`), without dumping.
- **Database subsetting** — dump only a filtered subset while following declared
  virtual references (including cyclic and polymorphic ones), keeping the result
  referentially consistent (`subset::SubsetEngine`).

## Stack

- **Rust 2021** (built with rustc 1.97)
- **clap** (CLI), **serde** + **serde_yaml** (config, `deny_unknown_fields`)
- **bson**, **sha2** (deterministic hashing), **flate2** (gzip), **regex**, **chrono**
- **Storage**: local directory (always compiled); **S3** (`aws-sdk-s3`),
  **Azure Blob** (`azure_storage_blobs`), **SFTP** (`ssh2`) behind optional cargo
  features (`s3`, `azure`, `ssh`).
- **MongoDB** access sits behind `MongoSource` / `MongoSink` traits with two
  implementations: an in-memory fake used by the unit tests, and a real
  `MongoDriver` backed by the `mongodb` crate (behind the optional `mongo`
  feature), covered by live integration tests. See
  [`.spectial/regeneration-gaps.md`](.spectial/regeneration-gaps.md).

## Install

Requires the Rust toolchain (`rustup`, `cargo`). If you don't have it:

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
. "$HOME/.cargo/env"
```

Then build the project:

```sh
cargo build
```

## Run (development)

```sh
cargo run -- list-transformers
cargo run -- show-transformer random_int
cargo run -- --config leafmask.yaml list-dumps
cargo run -- --config leafmask.yaml show-dump latest
cargo run -- --config leafmask.yaml delete --retain-recent 5 --dry-run
```

The config file is located via `--config` or the `LEAFMASK_CONFIG` environment
variable. A minimal directory-storage config:

```yaml
storage:
  type: directory
  path: ./dumps
```

Cloud backends require building with their feature, e.g.:

```sh
cargo run --features s3 -- --config leafmask.yaml list-dumps
```

The MongoDB commands need the `mongo` feature and a `mongodb.uri` in config (or
`--uri` / `LEAFMASK_MONGO_URI`):

```sh
cargo run --features mongo -- --config leafmask.yaml dump --include-db shop --gzip
cargo run --features mongo -- --config leafmask.yaml validate --data \
    --database shop --collection users --transformed-only
cargo run --features mongo -- --config leafmask.yaml restore latest --dependency-order
```

Config additions for MongoDB work:

```yaml
common:
  tmp_dir: ./tmp        # required for `dump`
  salt: pepper          # stable seed for deterministic transformations
mongodb:
  uri: mongodb://localhost:27017
dump:
  transformation:
    - collection: users
      transformers:
        - field: email
          name: random_email   # uniqueness-preserving (safe under a unique index)
```

## Test

The full suite is unit-testable without any external service:

```sh
cargo test
```

(94 tests, all green. The optional backends additionally compile-check with
`cargo check --features s3`, `--features azure`, `--features ssh`.)

The real MongoDB adapter has live integration tests. With a MongoDB reachable
(`docker run -d -p 27017:27017 mongo:7`):

```sh
cargo test --features mongo            # 94 unit + 4 integration tests
```

The URI defaults to `mongodb://localhost:27017`, overridable via
`LEAFMASK_MONGO_URI`.

## Build for production

```sh
cargo build --release
# binary at ./target/release/leafmask
./target/release/leafmask list-transformers
```

To include one or more optional backends in the release binary:

```sh
cargo build --release --features "s3 azure ssh"
```

## Product reference

The feature set, dependency graph, per-feature acceptance criteria, and access
rules live in [`.spectial/`](.spectial/). Regeneration assumptions and the one
known stub (the real MongoDB driver adapter) are recorded in
[`.spectial/regeneration-gaps.md`](.spectial/regeneration-gaps.md).
