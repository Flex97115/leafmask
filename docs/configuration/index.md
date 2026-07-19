# Configuration overview

Every Leafmask command reads a single YAML file. Point to it with `--config`, or
set the `LEAFMASK_CONFIG` environment variable:

```sh
leafmask --config ./leafmask.yaml list-dumps
# or
export LEAFMASK_CONFIG=./leafmask.yaml
leafmask list-dumps
```

## Top-level structure

```yaml
common: { ... }            # temp dir + transformer salt
mongodb: { ... }           # connection URI
storage: { ... }           # where dumps are written/read
dump:                      # what to anonymize while dumping
  transformation: [ ... ]
restore: { ... }           # insert-error tolerance + hook scripts
custom_transformers: [ ... ]   # your own transformers
resolved_warnings: [ ... ]     # validation warnings you've acknowledged
```

| Key | Purpose | Reference |
| --- | --- | --- |
| `common` | working directory and the deterministic salt | [below](#common) |
| `mongodb` | how to reach the deployment | [MongoDB connection](mongodb.md) |
| `storage` | pluggable dump storage backend | [Storage backends](storage.md) |
| `dump.transformation` | per-collection, per-field anonymization | [Transformations](transformations.md) |
| `restore` | tolerated insert errors and pre/post scripts | [Restore](restore.md) |
| `custom_transformers` | external/template transformers | [Transformations](transformations.md#custom-transformers) |
| `resolved_warnings` | warning ids to stop reporting | [validate](../commands.md#validate) |

!!! warning "Unknown keys are rejected"
    Leafmask validates the config strictly — a misspelled or unexpected key is an
    error, not a silent no-op. This catches typos before they cost you a bad
    dump.

## `common`

```yaml
common:
  tmp_dir: ./tmp        # required before `dump` runs; fails fast if missing
  salt: my-stable-seed  # seeds every deterministic transformer
```

- **`tmp_dir`** — a working directory. `dump` refuses to run without it.
- **`salt`** — the seed shared by all deterministic transformers. The **same
  input + same salt always produces the same output**, on any machine, across
  runs. Keep it stable to preserve referential values between dumps; rotate it to
  reshuffle all generated data. Defaults to `leafmask` if omitted; an empty
  string is treated as unset (the default salt is used, with a warning when
  transformations are configured).

## Environment interpolation

Any `${VAR}` in the file is replaced with the environment variable's value
**before** parsing. Perfect for secrets you don't want in the file:

```yaml
mongodb:
  uri: ${MONGO_URI}
storage:
  type: s3
  bucket: my-dumps
  access_key_id: ${AWS_ACCESS_KEY_ID}
  secret_access_key: ${AWS_SECRET_ACCESS_KEY}
  # endpoint: http://minio:9000  # override for S3-compatible services (MinIO, GCS, …)
```

```sh
export MONGO_URI="mongodb://user:pass@db:27017/?authSource=admin"
leafmask --config leafmask.yaml dump
```

An undefined variable expands to an empty string (matching shell `os.ExpandEnv`
semantics).

## Overrides

A few settings can be overridden per invocation, taking precedence over the file:

| Flag / env | Overrides |
| --- | --- |
| `--config`, `LEAFMASK_CONFIG` | config file location |
| `--uri`, `LEAFMASK_MONGO_URI` | `mongodb.uri` |

Command-specific flags (filters, `--gzip`, batch sizes, …) are documented under
[Commands](../commands.md).
