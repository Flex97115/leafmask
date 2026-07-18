# Regeneration gaps — Leafmask (Go → Rust)

Each entry is a place where the product file did not fully determine the
implementation, or where the target environment prevented reaching green
integration tests. Every entry is a candidate improvement for the extraction
skill or the schema.

## Architectural decisions (target-stack porting)

- feature: storage.s3-backend, storage.azure-backend, storage.ssh-backend
  missing: the product file mandates a common `Storager` interface but does not
    constrain how cloud backends are built/compiled in the target stack.
  assumed: cloud backends are put behind default-off cargo features
    (`s3`, `azure`, `ssh`) so the core build stays lean and fully unit-testable.
    Backend selection returns a clear "backend not compiled in; rebuild with
    --features <x>" error when the feature is absent, satisfying "fail fast with
    a clear error". Real cloud round-trip tests cannot run in this environment
    (no S3/Azure/SFTP credentials).

- feature: dump.create, restore.database, validate.schema-diff
  missing: no MongoDB deployment is available in the regeneration environment.
  assumed: all MongoDB access sits behind a `MongoSource` / `MongoSink` trait.
    Dump/restore/subset/validate logic is unit-tested against an in-memory fake
    implementation; the real `mongodb`-crate driver impl is behind the default-off
    `mongo` feature and is not exercised by tests here.

## Data-model / behaviour assumptions

- feature: subset.virtual-references
  missing: the source expresses polymorphic reference targets as free-form
    expressions (`polymorphic_exprs`).
  assumed: modelled as structured `field == value -> collection` discriminator
    cases, which covers the documented use of choosing a target collection by a
    type field. A full expression language was not reproduced for this edge.
