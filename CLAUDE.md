See @README.md for the project overview, config format, and command list.

# Build & test

- `cargo build` — default features (directory storage only, no MongoDB driver). No system deps.
- `cargo test` — fast unit suite, no external services.
- `cargo test --features mongo` — adds live integration tests against MongoDB; needs
  `docker run -d -p 27017:27017 mongo:7` first (URI: `LEAFMASK_MONGO_URI`, defaults to
  `mongodb://localhost:27017`).
- `make lint` — `cargo fmt --check` + `cargo clippy --all-targets --features full -- -D warnings`.
  Run this before considering any change done; CI runs the same two commands.
- `cargo build --features full` (or `make build`) needs `cmake`, `pkg-config`, `perl`, `make` on
  the system (OpenSSL is vendored for the `ssh` backend). `s3`, `azure`, `ssh`, `mongo` are each
  independently feature-gated; commands behind `mongo` (`dump`, `restore`, `validate`, `bench`)
  fail fast with an actionable message when the feature isn't compiled in, rather than silently
  no-opping.

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
