# Commands

```
leafmask [--config <file>] [--uri <mongodb-uri>] <command> [flags]
```

Global options apply to every command:

| Option | Env | Description |
| --- | --- | --- |
| `--config <file>` | `LEAFMASK_CONFIG` | path to the YAML config file |
| `--uri <uri>` | `LEAFMASK_MONGO_URI` | MongoDB URI, overriding `mongodb.uri` |

!!! info "Feature-gated commands"
    `dump`, `restore`, `validate`, and `bench` need a binary built with the
    `mongo` feature (included in the prebuilt binaries and Docker image).
    Without it they appear in `--help` but error clearly when invoked.

---

## Catalog

### `list-transformers`

List every available transformer — built-in and any declared under
`custom_transformers` — with a short description.

```sh
leafmask list-transformers
leafmask --config leafmask.yaml list-transformers   # include custom ones
```

### `show-transformer`

Show one transformer's full documentation: parameters, their types, and whether
they're required.

```sh
leafmask show-transformer random_int
```

An unknown name fails with a clear "not found" error.

---

## Dump lifecycle

### `dump`

Create a logical dump of the configured MongoDB into the configured storage.
Documents are streamed from a server-side cursor, transformed inline, and
spooled to `common.tmp_dir` before upload — memory stays bounded per document,
never per collection, so multi-gigabyte / 10M+ document collections dump fine.
Each collection is stored as a plain concatenation of BSON documents
(`mongodump`-style framing) plus a small structure blob (indexes, validator,
options).

```sh
leafmask --config leafmask.yaml dump --include-db shop --gzip
```

| Flag | Description |
| --- | --- |
| `--gzip` | compress collection data |
| `--include-db <name>` | restrict to these databases (repeatable) |
| `--exclude-db <name>` | skip these databases (repeatable) |
| `--include-collection <name>` | restrict to these collections (repeatable) |
| `--exclude-collection <name>` | skip these collections (repeatable) |
| `--no-indexes` | exclude index definitions and collection options |
| `--jobs <n>` | number of parallel jobs (accepted; runs sequentially) |

Each `--include-*`/`--exclude-*` flag falls back to a YAML list under `dump:`
when omitted (or passed with no values) — useful once there are too many
databases/collections to spell out on the command line. A non-empty flag on
the command line overrides the matching YAML list entirely (same precedence
as `--uri` overriding `mongodb.uri`):

```yaml
dump:
  include_databases: [shop, billing]
  exclude_databases: [logs]
  include_collections: [users, orders]
  exclude_collections: [sessions]
```

| Flag | Config fallback |
| --- | --- |
| `--include-db` | `dump.include_databases` |
| `--exclude-db` | `dump.exclude_databases` |
| `--include-collection` | `dump.include_collections` |
| `--exclude-collection` | `dump.exclude_collections` |

Requires `common.tmp_dir`; without it the dump fails fast. Prints the new dump
id.

### `list-dumps`

List all dumps in the configured storage with their id, status, and size. An
empty storage lists nothing (no error).

```sh
leafmask --config leafmask.yaml list-dumps
```

### `show-dump`

Show a dump's metadata and table of contents (databases, collections, indexes,
restore order). Accepts an id or `latest`.

```sh
leafmask --config leafmask.yaml show-dump latest
leafmask --config leafmask.yaml show-dump 20260718T185958Z
```

A nonexistent id fails with a clear error.

### `restore`

Restore a dump into the configured MongoDB. Accepts an id or `latest`.

```sh
leafmask --config leafmask.yaml restore latest --dependency-order
```

| Flag | Default | Description |
| --- | --- | --- |
| `<id>` | — | dump id to restore, or `latest` (positional, required) |
| `--include-collection <name>` | | restrict to these collections (repeatable) |
| `--exclude-collection <name>` | | skip these collections (repeatable) |
| `--include-index <name>` | | restrict to these indexes (repeatable) |
| `--exclude-index <name>` | | skip these indexes (repeatable) |
| `--batch-size <n>` | `1000` | documents per bulk-insert batch (one server round-trip per batch) |
| `--ordered` | off | use ordered bulk writes (stop each batch at its first error) |
| `--dependency-order` | off | create indexes/validators after documents |
| `--exit-on-error` | off | abort the whole restore on a non-excluded error |
| `--clean` | off | drop each restored collection before recreating it from the dump |
| `--jobs <n>` | `1` | number of collections to restore concurrently |

Documents are streamed out of the dump and bulk-inserted `--batch-size` at a
time, so memory stays flat however large the collection is. A non-excluded
insert error fails that collection and the restore moves on to the next one
(the command then exits non-zero listing the failed collections); with
`--exit-on-error` it aborts everything immediately.

Each `--include-*`/`--exclude-*` flag falls back to a YAML list in the
[`restore`](configuration/restore.md#filtering) config section when omitted;
a non-empty flag on the command line overrides the matching YAML list
entirely. `--clean` ORs with `clean: true` in the config section — either one
turns it on.

Tolerated insert errors, pre/post scripts, and filtering defaults all come
from the [`restore`](configuration/restore.md) config section. Prints a
summary (`inserted`, `skipped`, `indexes`).

### `delete`

Delete a dump by id, or prune many by a retention policy.

```sh
# a single dump
leafmask --config leafmask.yaml delete --id 20260718T185958Z

# keep the 7 most recent, preview first
leafmask --config leafmask.yaml delete --retain-recent 7 --dry-run
```

| Flag | Description |
| --- | --- |
| `--id <id>` | delete this specific dump |
| `--retain-recent <n>` | keep only the N most recent completed dumps |
| `--before-date <rfc3339>` | delete dumps created before this date |
| `--prune-failed` | delete dumps that didn't complete successfully |
| `--prune-unsafe` | also delete unknown/in-progress dumps |
| `--dry-run` | report what would be deleted without deleting |

---

## Validation

### `validate`

Preview transformations against a live sample as a before/after diff — without
producing a dump.

```sh
leafmask --config leafmask.yaml validate --data \
  --database shop --collection users --rows-limit 20
```

| Flag | Default | Description |
| --- | --- | --- |
| `--data` | — | run the preview (required) |
| `--database <name>` | — | database to sample from |
| `--collection <name>` | — | collection to sample from |
| `--rows-limit <n>` | `10` | maximum documents to sample |
| `--format <fmt>` | `text` | output format: `text` or `json` |
| `--table-format <fmt>` | `vertical` | layout: `vertical` or `horizontal` |
| `--transformed-only` | off | only show fields that have a transformer |
| `--strict` | off | fail on any unresolved validation warning |

An invalid `--format` or `--table-format` fails fast, before any database work.

---

## Benchmarking

### `bench`

Measure real dump/restore throughput against a live MongoDB deployment. It
seeds synthetic "client" documents into a temporary `leafmask_bench` database,
times a gzip dump to local directory storage, drops the database, times a
restore, verifies the document count, and cleans everything up. The seeding
step itself is not counted in the timings.

```sh
leafmask bench --uri mongodb://localhost:27017 --markdown
```

| Flag | Default | Description |
| --- | --- | --- |
| `--uri <uri>` | — | MongoDB URI (or use the global `--uri` / `LEAFMASK_MONGO_URI`) |
| `--sizes <n,n,...>` | `100000,1000000` | document counts to benchmark, comma-separated |
| `--estimate <n>` | `10000000` | an additional row, linearly extrapolated from the largest measured size (marked *(estimated)*, not actually run) |
| `--markdown` | off | print the results as a markdown table instead of plain text |
| `--keep` | off | keep the `leafmask_bench` database for the last size instead of dropping it |

As a safety guard, `bench` refuses to touch a `leafmask_bench` database it
didn't create itself (detected via a marker collection), so it never
overwrites unrelated data. See [Benchmarks](benchmarks.md) for full results
and machine specs.
