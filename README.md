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

Runnable CLI commands today (they only touch config + storage, no MongoDB):

- **`list-transformers`** — list every available transformer (built-in and any
  custom ones declared in the config), each with a short description.
- **`show-transformer <name>`** — show one transformer's full documentation:
  parameters, their types, and whether they are required.
- **`list-dumps`** — list all dumps in the configured storage with id and status.
- **`show-dump <id|latest>`** — show a dump's table of contents and metadata.
- **`delete`** — delete a dump by id, or prune by a retention policy
  (`--retain-recent`, `--before-date`, `--prune-failed`, `--prune-unsafe`), with
  `--dry-run`.

Feature-complete as **library drivers** (fully implemented and unit-tested
against an in-memory MongoDB), pending the concrete `mongodb`-crate adapter
before they can run against a live server from the CLI — see
[`.spectial/regeneration-gaps.md`](.spectial/regeneration-gaps.md):

- **Dump** — logical dump with collection filters, inline BSON binary,
  index/option capture, optional gzip, transformations + subsetting applied
  while streaming (`dump::Dump`).
- **Restore** — restore a dump into a target MongoDB with filters, ordering,
  batching, error exclusions, and pre/post scripts (`restore::Restore`).
- **Transformation preview** (`validate --data`) — before/after diff of the
  configured transformations over a sample, in text/JSON, without dumping
  (`validate::preview`).
- **Database subsetting** — dump only a filtered subset while following declared
  virtual references (including cyclic and polymorphic ones) so the result stays
  referentially consistent (`subset::SubsetEngine`).

## Stack

- **Rust 2021** (built with rustc 1.97)
- **clap** (CLI), **serde** + **serde_yaml** (config, `deny_unknown_fields`)
- **bson**, **sha2** (deterministic hashing), **flate2** (gzip), **regex**, **chrono**
- **Storage**: local directory (always compiled); **S3** (`aws-sdk-s3`),
  **Azure Blob** (`azure_storage_blobs`), **SFTP** (`ssh2`) behind optional cargo
  features (`s3`, `azure`, `ssh`).
- **MongoDB** access sits behind `MongoSource` / `MongoSink` traits with an
  in-memory implementation used by the tests. A concrete adapter backed by the
  real `mongodb` crate (behind the optional `mongo` feature) is a documented
  extension point that this regeneration did not implement — see
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

## Test

The full suite is unit-testable without any external service:

```sh
cargo test
```

(94 tests, all green. The optional backends additionally compile-check with
`cargo check --features s3`, `--features azure`, `--features ssh`, `--features mongo`.)

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
