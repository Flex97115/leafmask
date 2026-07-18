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
  status: RESOLVED — MongoDB access sits behind `MongoSource` / `MongoSink`
    traits, with two implementations: the in-memory `InMemoryMongo` (backing the
    unit tests) and the real `MongoDriver` (async `mongodb` crate driven from the
    sync traits via a Tokio runtime), compiled with the `mongo` feature. The
    adapter is exercised by live integration tests in `tests/mongo_integration.rs`
    (dump→drop→restore round trip, duplicate-key error mapping, inline
    transformation on a real read) run against a real MongoDB.
  remaining: reads use a plain `find` — point-in-time snapshot sessions require a
    replica set (a standalone `mongod` rejects them), so the "session snapshot"
    consistency of `dump.create` is available only against a replica set. The CLI
    `dump`/`restore`/`validate --data` subcommands are wired onto `MongoDriver`
    (behind `--features mongo`) and verified end-to-end against a live server; a
    non-`mongo` build shows them in `--help` but errors clearly when invoked.

## Data-model / behaviour assumptions

- feature: subset.virtual-references
  missing: the source expresses polymorphic reference targets as free-form
    expressions (`polymorphic_exprs`).
  assumed: modelled as structured `field == value -> collection` discriminator
    cases, which covers the documented use of choosing a target collection by a
    type field. A full expression language was not reproduced for this edge.

- feature: transform.builtin-transformers
  missing: the source ships a Go text/template transformer and a very large
    faker library (dozens of locale-aware generators).
  assumed: `template` supports `{{ field }}` document-field substitution rather
    than the full Go template language; the generator set (person, company,
    email, int/float/date/bytes/objectId, noise, masking, regexp, replace,
    set_null, hash) is representative and deterministic, not the full catalog.

- feature: transform.custom-transformers
  missing: the source command-driver protocol is BSON/length-framed with a
    metadata handshake.
  assumed: the command driver exchanges one JSON value per line over
    stdin/stdout with a persistent process. Declared parameters are surfaced in
    the catalog but not forwarded to the process handshake.

- feature: transform.transformation-condition
  missing: the source uses a full expression language (Go expr) for `when`.
  assumed: a focused evaluator supporting field references, `== != > < >= <=`,
    `and`/`or`, and bare-field truthiness (no parentheses/arithmetic), which
    covers the documented conditional-transformation use.

- feature: transform.apply-transformations
  missing: the collection `query` is a MongoDB filter document.
  assumed: pushed to MongoDB at read time in the real dump; the offline
    should_include() matcher approximates it with equality-only matching for
    testing without a database.

- feature: restore.database, dump.create
  missing: parallel job execution and precise ordered/unordered bulk-write
    error semantics.
  assumed: restore/dump run sequentially with configurable batch sizes; the
    ordered/parallel_jobs options are accepted but execution is sequential (a
    performance detail, not behavioural). Real session snapshots and bulk writes
    require the mongo feature + a live server.
