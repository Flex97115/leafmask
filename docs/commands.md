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
    `dump`, `restore`, and `validate` need a binary built with the `mongo`
    feature (included in the prebuilt binaries and Docker image). Without it they
    appear in `--help` but error clearly when invoked.

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
Transformations and per-collection query filters are applied while streaming.

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
| `--batch-size <n>` | `1000` | documents per bulk-insert batch |
| `--ordered` | off | use ordered bulk writes |
| `--dependency-order` | off | create indexes/validators after documents |
| `--exit-on-error` | off | abort the whole restore on a non-excluded error |

Each `--include-*`/`--exclude-*` flag falls back to a YAML list in the
[`restore`](configuration/restore.md#filtering) config section when omitted;
a non-empty flag on the command line overrides the matching YAML list
entirely.

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
