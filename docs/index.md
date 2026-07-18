# Leafmask

**Leafmask** is a stateless command-line tool for **logical MongoDB dumping**,
**deterministic data anonymization**, **referentially-intact subsetting**, and
**restoration**.

Point it at a MongoDB deployment, describe how each field should be masked in a
single YAML file, and Leafmask streams the collections out — transforming
documents inline — into the storage backend of your choice. Restore any dump
back into a target database when you need a realistic, anonymized copy of
production for staging, CI, or local development.

<div class="grid cards" markdown>

-   :material-database-export: **Logical dumps**

    Dump collections, indexes, and validators through the MongoDB driver — no
    wrapped `mongodump`. Filter by database/collection, compress with gzip.

-   :material-shield-account: **Deterministic anonymization**

    A library of built-in transformers (hashing, synthetic people, emails,
    noise, masking, …). The same input always yields the same output, so
    references stay consistent across runs.

-   :material-sitemap: **Referential subsetting**

    Declare relationships between collections (MongoDB has no enforced foreign
    keys) and dump a consistent subset — cyclic and polymorphic references
    included.

-   :material-database-import: **Restore**

    Restore a dump by id or `latest` with filters, dependency ordering,
    tolerated insert errors, and pre/post `mongosh` hooks.

</div>

## Why Leafmask

- **Stateless.** No server, no database of its own. Access control is delegated
  to the MongoDB and storage credentials you pass to the CLI.
- **One config file.** A single YAML file with `${ENV}` interpolation drives
  every command. Unknown keys are rejected, not silently ignored.
- **Pluggable storage.** Local directory out of the box; S3, Azure Blob, and
  SFTP behind build features — selected purely by config.
- **Preview before you run.** `validate --data` shows a before/after diff of
  your transformations against a live sample, without producing a dump.

## A taste

```yaml title="leafmask.yaml"
common:
  tmp_dir: ./tmp
  salt: change-me                 # (1)!
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
          name: random_email      # (2)!
        - field: name
          name: random_person
```

1.  The salt seeds every deterministic transformer. Keep it stable to get the
    same anonymized values across dumps; change it to reshuffle everything.
2.  `random_email` produces distinct, stable addresses — safe under a unique
    index (unlike `masking`, which would collapse values).

```sh
leafmask --config leafmask.yaml dump
leafmask --config leafmask.yaml validate --data --database shop --collection users
leafmask --config leafmask.yaml restore latest
```

Ready? Head to [Installation](installation.md), then the
[Getting started](getting-started.md) walkthrough.

!!! note "Feature builds"
    The MongoDB commands (`dump`, `restore`, `validate --data`) and the cloud
    storage backends are compiled in via Cargo features. Prebuilt binaries and
    the Docker image ship with **everything** enabled. See
    [Installation](installation.md#from-source) for building a leaner binary.
