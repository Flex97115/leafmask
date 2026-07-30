See @README.md for the project overview, config format, and command list.

# Build & test

- `cargo build` — default features (directory storage only, no MongoDB driver). No system deps.
- `cargo test` — fast unit + property suites, no external services.
- `cargo test --features mongo` — adds live integration tests against MongoDB; needs
  `docker run -d -p 27017:27017 mongo:7` first (URI: `LEAFMASK_MONGO_URI`, defaults to
  `mongodb://localhost:27017`).
- `make test-mongo-matrix` — the integration suite against every supported MongoDB version.
- `make test-storage` — S3/Azure backends against real MinIO/Azurite containers.
- `make lint` — `cargo fmt --check` + `cargo clippy --all-targets --features full -- -D warnings`.
  Run this before considering any change done; CI runs the same two commands.
- `cargo build --features full` (or `make build`) needs `cmake`, `pkg-config`, `perl`, `make` on
  the system (OpenSSL is vendored for the `ssh` backend). `s3`, `azure`, `ssh`, `mongo` are each
  independently feature-gated; commands behind `mongo` (`dump`, `restore`, `validate`, `bench`)
  fail fast with an actionable message when the feature isn't compiled in, rather than silently
  no-opping.

# Testing policy

Leafmask writes anonymized copies of production data and restores them into live databases. A
bug here is not a crashed request — it is unmasked PII in a staging database, or a restore that
silently drops documents. Unit tests against `InMemoryMongo` prove the *logic*; they cannot prove
the thing that actually breaks, which is the boundary: the real driver, the real server, the real
storage backend, the real byte format. **Integration tests are not optional extras here — treat
them as part of the feature, and keep them passing.**

The suite has four layers. Know which one a change belongs in:

| Layer | Where | Runs against | Catches |
|---|---|---|---|
| Unit | `#[cfg(test)]` in `src/` | `InMemoryMongo`, temp dirs | logic, config parsing, transformer semantics |
| Property | `tests/property_*.rs` | pure, no services | invariants across inputs nobody enumerated |
| Integration | `tests/mongo_integration.rs`, `tests/{s3,azure}_integration.rs` | real MongoDB / MinIO / Azurite | driver behaviour, wire formats, streaming paths |
| Version matrix | `tests/mongo_version_matrix.rs` | every supported MongoDB release | server contract drift across versions |

Rules that keep this working:

- **Anything touching `MongoDriver`, a `Storage` backend, or the on-disk dump format needs an
  integration test.** A unit test against a fake proves the fake agrees with you, nothing more.
  `InMemoryMongo` has no wire protocol, no BSON encoder, no write-concern semantics, and no error
  codes — precisely the surfaces that break.
- **Server behaviour Leafmask depends on belongs in `mongo_version_matrix.rs`, not
  `mongo_integration.rs`.** Error codes, catalog shapes, BSON round-trip fidelity, batch-write
  semantics: assert them there, so every supported release is checked. `mongo_integration.rs` is
  for Leafmask's *own* behaviour against a live server.
- **Supported MongoDB versions are declared in exactly three places, and they must agree:**
  `SUPPORTED_SERVER_VERSIONS` in `tests/support/mod.rs`, the `integration` matrix in
  `.github/workflows/ci.yml`, and `DEFAULT_VERSIONS` in `scripts/test-mongo-matrix.sh`. Adding a
  future release means adding it to all three and making the suite pass — the
  `server_under_test_is_a_supported_release` test fails loudly if CI is ever pointed at a release
  that was never vetted, so support is always something someone verified rather than assumed.
- **Never gate a whole test behind a version check.** Use `support::server_at_least(major, minor)`
  to gate an individual *assertion*. A test that quietly skips itself on a new server version is
  the exact regression the matrix exists to catch.
- **Use `support::TestDb`** for new MongoDB tests: unique database name per test, dropped on
  `Drop` so a failing assertion cannot leave residue for the next matrix leg.
- **A bug found by a property test gets a named unit-test regression too.** The property test
  finds it; the named test documents it and pins it against a specific input. (Both `noise_int`
  and `noise_date` overflows came in this way — see `src/transform/builtin.rs`.)
- **Determinism is the product promise, so it is a property, not an example.** Any new
  transformer is automatically covered by `tests/property_transformers.rs` via the registry —
  don't weaken those tests to make a transformer pass; fix the transformer.
- Property failures print a reproducible seed and are persisted to
  `tests/*.proptest-regressions`. **Commit those files** — they re-run the exact failing case
  first on every subsequent run.
- **Coverage is a blocking CI gate: 85% lines, 75% functions** (over unit + property + mongo
  integration). The floors live in two places that must agree: `MIN_LINE_COVERAGE` /
  `MIN_FUNCTION_COVERAGE` in the `Makefile`, and the `coverage` job env in
  `.github/workflows/ci.yml`. Verify with `make coverage` before pushing. **Never lower a floor
  to make a red build green** — a failing gate means tests were deleted or new code arrived
  untested; add the tests. Raise the floors when the real numbers rise.

# Architecture

- MongoDB access goes through the `MongoSource`/`MongoSink` traits (`src/mongo.rs`), never a
  wrapped `mongodump`/`mongorestore` — this lets dump/restore apply per-document transformation
  inline while streaming. Both traits are deliberately synchronous (no `async`) so the
  `InMemoryMongo` test fake needs no runtime; the real `MongoDriver` (behind `mongo`) blocks on
  its own Tokio runtime internally. Keep this boundary sync when touching either trait.
- Storage backends (`directory`/`s3`/`azure`/`ssh`) go through the `Storage` trait
  (`src/storage/mod.rs`, `Send + Sync`), selected at runtime from `storage.type` in config.
- A dump's data blob is a plain concatenation of BSON documents (mongodump-style framing), never
  one wrapper document — a single wrapper would cap a collection at BSON's i32/2 GiB limit and
  force it into memory. See `CollectionDataWriter`/`DocumentReader` in `src/dump/mod.rs`.
  The framing has no end marker, so a blob truncated *on a document boundary* is
  indistinguishable from a clean end of stream at the reader level. `CollectionMeta.document_count`
  is what closes that hole: restore compares it against the documents it actually read and fails
  the collection on a shortfall (`src/restore/database.rs`). Keep that check whenever you touch the
  restore read path — without it an interrupted upload restores as a silently short collection.
  Only a shortfall is an error: metadata predating the field decodes to `0` via `serde(default)`
  and must still restore.
- Transformers are deterministic: `HashEngine` (`src/hash.rs`) derives every pseudo-random output
  purely from `SHA-256(salt || input)` — no RNG, no time input, no shared mutable state. Same
  input always produces the same output, across runs, machines, and dump worker threads
  (`dump --jobs`).
- `.spectial/` holds the original product spec (feature list, dependency graph, acceptance
  criteria) this codebase was regenerated from via Spectial; `.spectial/regeneration-gaps.md`
  records the Rust/MongoDB porting decisions. Check it before assuming a gap from the original
  Greenmask/PostgreSQL feature set is a bug.

# Gotchas

- When adding or touching a CLI flag in `src/cli.rs`, verify it's actually read in the matching
  `cmd_*` function (or the options struct it builds) — `clap` will happily accept a flag that
  nothing downstream consumes. (`dump --jobs` was silently unused this way before it was wired up.)
- **Adding a key to the `dump:` or `restore:` config section means adding it to `DUMP_KEYS` /
  `RESTORE_KEYS` in `src/config.rs` too.** Those sections deserialize as raw `serde_yaml::Value`
  and are then refined by several narrow structs in `cli.rs`, so no single struct can carry
  `deny_unknown_fields` without rejecting the others' legitimate keys — the key lists stand in for
  it. Forget the list and your new key is rejected (loud, you'll hit it immediately); remove the
  check and a mistyped key is silently ignored, which for `dump.transformation` means **a
  successful dump containing completely unmasked data**. That is the single worst failure this
  tool has, so the check is not optional.
- There is deliberately no `validate:` config section — every `validate` option is a CLI flag and
  resolved warnings live in top-level `resolved_warnings`. It used to exist with no reader, so its
  keys were accepted and did nothing; it was removed. Don't add a raw section back without giving
  it a reader and a `*_KEYS` entry in the same change.
