<div align="center">

# 🍃 Leafmask

**Stateless CLI for logical MongoDB dumping, deterministic anonymization,
referentially-intact subsetting, and restoration.**

[![CI](https://github.com/Flex97115/leafmask/actions/workflows/ci.yml/badge.svg)](https://github.com/Flex97115/leafmask/actions/workflows/ci.yml)
[![Docs](https://img.shields.io/badge/docs-mkdocs--material-teal)](https://flex97115.github.io/leafmask/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-2021-orange?logo=rust)](https://www.rust-lang.org)

</div>

---

> ### 🌱 How this project came to be
>
> Leafmask was **generated with [Spectial](https://github.com/Flex97115/spectial)** —
> a product-regeneration toolkit that captures an application's feature set,
> dependency graph, and acceptance criteria into a portable `.spectial/` product
> file, then rebuilds it, feature by feature, in the stack of your choice.
>
> The product base was **extracted from [Greenmask](https://github.com/GreenmaskIO/greenmask)**,
> the excellent PostgreSQL dump & anonymization tool, which served as the
> conceptual foundation. Spectial ported that base to **Rust and MongoDB** — every
> feature here traces back to an entry in [`.spectial/`](.spectial/), reproduced
> with its own tests.

---

## What is Leafmask

Point Leafmask at a MongoDB deployment, describe how each field should be masked
in a single YAML file, and it streams the collections out — transforming
documents inline — into the storage backend of your choice. Restore any dump back
into a target database when you need a realistic, anonymized copy of production
for staging, CI, or local development.

- 🗄️ **Logical dumps** — collections, indexes, and validators via the driver
  (no wrapped `mongodump`); filter by db/collection, compress with gzip.
- 🛡️ **Deterministic anonymization** — a library of transformers (hashing,
  synthetic people, emails, noise, masking…). Same input → same output, so
  references stay consistent across runs.
- 🕸️ **Referential subsetting** — declare relationships (MongoDB has no enforced
  foreign keys) and dump a consistent slice — cyclic and polymorphic refs
  included.
- ♻️ **Restore** — by id or `latest`, with filters, dependency ordering,
  tolerated insert errors, and pre/post `mongosh` hooks.
- 🔌 **Pluggable storage** — local directory out of the box; S3, Azure Blob, and
  SFTP behind build features, selected purely by config.
- 🔍 **Preview before you run** — `validate --data` shows a before/after diff
  against a live sample, without producing a dump.

## Compatibility

Every release below runs the **full integration suite** in CI — dump, restore,
subsetting, BSON round-trip fidelity, error codes, and index semantics — so
support is verified on each commit, not assumed:

| MongoDB | Tested against | Status |
| --- | --- | --- |
| **6.0** | `mongo:6.0` | ✅ full suite in CI |
| **7.0** | `mongo:7.0` | ✅ full suite in CI |
| **8.0** | `mongo:8.0` | ✅ full suite in CI |

CI runs the `mongo`-gated suite once per version
([matrix](.github/workflows/ci.yml)), and
[`tests/mongo_version_matrix.rs`](tests/mongo_version_matrix.rs) pins the
server behaviour Leafmask depends on. Reproduce locally with Docker:

```sh
make test-mongo-matrix          # every supported version
./scripts/test-mongo-matrix.sh 8.0   # just one
```

Pointing the suite at a release that isn't declared supported fails loudly
rather than quietly implying it works. See
[supported versions](#supported-mongodb-versions) for how to add a future
release.

## Performance

Measured with `leafmask bench` — synthetic "client" documents (~10 fields:
identity, contact, address, status, dates, counters) dumped with gzip to local
directory storage and restored into an empty database. Figures exclude any S3
network cost; storage is a local directory. Machine: Apple M3, 24 GiB RAM —
MongoDB 7 in Docker, local.

| Documents | Dump | Restore | Dump size | Docs/s (dump) |
|---|---|---|---|---|
| 100,000 | 0.6s | 0.9s | 2.4 MiB | 175,799 |
| 1,000,000 | 5.3s | 9.0s | 24.1 MiB | 189,319 |
| 10,000,000 *(estimated)* | 52.8s | 1m 30s | 241.4 MiB | 189,319 |

The 10M row is a linear extrapolation from the 1M run. Reproduce with:

```sh
leafmask bench --uri mongodb://localhost:27017 --markdown
```

## Quick start

```yaml title="leafmask.yaml"
common:
  tmp_dir: ./tmp
  salt: change-me
mongodb:
  uri: mongodb://localhost:27017
storage:
  type: directory
  path: ./dumps
dump:
  transformation:
    - collection: users
      transformers:
        - field: email
          name: random_email      # distinct & deterministic → safe under a unique index
        - field: name
          name: random_person
```

```sh
leafmask --config leafmask.yaml validate --data --database shop --collection users
leafmask --config leafmask.yaml dump
leafmask --config leafmask.yaml restore latest
```

📖 **Full documentation: <https://flex97115.github.io/leafmask/>** — installation,
a getting-started walkthrough, the complete configuration reference, and every
transformer.

## Install

### Install script

Detects your OS/arch, downloads the matching prebuilt binary from the latest
GitHub Release, verifies its checksum, and installs it:

```sh
curl -fsSL https://raw.githubusercontent.com/Flex97115/leafmask/main/install.sh | sh
```

Overrides: `LEAFMASK_VERSION`, `LEAFMASK_INSTALL_DIR`, `LEAFMASK_REPO`.
Prebuilt targets: Linux and macOS on x86_64 and arm64.

### Docker

```sh
docker run --rm ghcr.io/flex97115/leafmask:latest --help
```

### From source

Requires the [Rust toolchain](https://rustup.rs). The lean core needs no system
libraries; the full binary (MongoDB + all storage backends) needs `cmake`,
`perl`, `make` (OpenSSL is vendored):

```sh
make build-core   # core + directory storage
make build        # everything: MongoDB + S3 + Azure + SSH
make install      # -> /usr/local/bin (override with PREFIX=)
```

See the [installation guide](https://flex97115.github.io/leafmask/installation/)
for the full feature matrix.

## Commands

| | |
| --- | --- |
| `dump` | create a logical, anonymized dump |
| `restore <id\|latest>` | restore a dump into MongoDB |
| `validate --data` | preview transformations without dumping |
| `list-dumps` / `show-dump` / `delete` | manage stored dumps |
| `list-transformers` / `show-transformer` | explore the transformer catalog |

Full reference: [Commands](https://flex97115.github.io/leafmask/commands/).

## Development

```sh
cargo test                    # unit + property tests, no external services
cargo test --features mongo   # + live integration tests (needs MongoDB on :27017)
make test-mongo-matrix        # the integration suite on every supported MongoDB
make test-storage             # S3/Azure against real MinIO/Azurite containers
make lint                     # cargo fmt --check + clippy -D warnings (full features)
```

The MongoDB adapter has live integration tests. Start a database with
`docker run -d -p 27017:27017 mongo:7` (URI defaults to
`mongodb://localhost:27017`, overridable via `LEAFMASK_MONGO_URI`).

### Testing layers

Leafmask writes anonymized data into live databases, so the tests are built to
catch boundary bugs, not just logic bugs:

| Layer | Runs against | Catches |
| --- | --- | --- |
| **Unit** (`src/`) | in-memory fakes | logic, config, transformer semantics |
| **Property** (`tests/property_*.rs`) | pure, no services | invariants across generated inputs — dump-format round trips, transformer determinism |
| **Integration** (`tests/*_integration.rs`) | real MongoDB, MinIO, Azurite | driver behaviour, wire formats, streaming paths |
| **Version matrix** (`tests/mongo_version_matrix.rs`) | every supported MongoDB release | server contract drift — error codes, catalog shapes, BSON fidelity |

### Supported MongoDB versions

The tested releases are listed under [Compatibility](#compatibility). Adding a
future release is a three-line change — `SUPPORTED_SERVER_VERSIONS` in
[`tests/support/mod.rs`](tests/support/mod.rs), the `integration` matrix in
[`ci.yml`](.github/workflows/ci.yml), and `DEFAULT_VERSIONS` in
[`scripts/test-mongo-matrix.sh`](scripts/test-mongo-matrix.sh) — after which
the whole suite must pass against it before the release is claimed as
supported.

### Coverage

Coverage is a **blocking CI gate**: at least **85% line** and **75% function**
coverage, measured over the unit, property, and MongoDB integration suites.
Check it locally with `make coverage` (needs
[cargo-llvm-cov](https://github.com/taiki-e/cargo-llvm-cov) and a MongoDB on
`:27017`). Raise the floors in the Makefile and `ci.yml` when the real numbers
rise; never lower them to turn a red build green.

## Releasing

Publishing is driven by git tags. Pushing a `vX.Y.Z` tag runs
[`release.yml`](.github/workflows/release.yml): it builds `--features full`
binaries for x86_64/arm64 on Linux and macOS, creates a GitHub Release with
checksums, and pushes a multi-arch image to
`ghcr.io/flex97115/leafmask:{version,latest}`.

```sh
git tag v0.1.0 && git push origin v0.1.0
```

On every push/PR, [`ci.yml`](.github/workflows/ci.yml) runs formatting, clippy,
and the full test suite (including live MongoDB integration tests against a
`mongo:7` service). Documentation is deployed to GitHub Pages by
[`docs.yml`](.github/workflows/docs.yml).

## Product reference

The feature set, dependency graph, per-feature acceptance criteria, and access
rules live in [`.spectial/`](.spectial/) — the Spectial product base this
implementation was regenerated from. Regeneration assumptions and target-stack
porting decisions are recorded in
[`.spectial/regeneration-gaps.md`](.spectial/regeneration-gaps.md).

### 🕸️ Explore the feature graph

Spectial renders the whole product as an interactive dependency graph — every
feature, what it depends on, and which files implement it. Click a node to read
its specification.

**▶ [Open the live feature graph](https://flex97115.github.io/leafmask/graph.html)**

Leafmask currently maps to **33 features** across 11 domains (11 infrastructure,
10 internal, 12 user-facing). `dump.storage-format` and `mongo.access-layer` are
the widest blast radius — change either and most of the product moves.

The graph is generated, not hand-drawn: `spectial:extract` writes
`.spectial/graph.html` alongside [`graph.json`](.spectial/graph.json),
[`graph.mmd`](.spectial/graph.mmd), and a
[report](.spectial/GRAPH_REPORT.md) of god nodes and leaf features.

## Credits

- **[Spectial](https://github.com/Flex97115/spectial)** — the regeneration
  toolkit that produced this project.
- **[Greenmask](https://github.com/GreenmaskIO/greenmask)** — the original
  PostgreSQL anonymization tool this product base was extracted from.

## License

[Apache-2.0](LICENSE).
