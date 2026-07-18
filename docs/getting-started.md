# Getting started

This walkthrough takes you from an empty machine to a working
**dump → preview → restore** cycle in a few minutes. You'll need
[Leafmask installed](installation.md) (with the `mongo` feature) and a MongoDB to
point at.

## 1. Start a MongoDB

Any reachable deployment works. For a throwaway local one:

```sh
docker run -d --name mongo -p 27017:27017 mongo:7
```

Seed a little data to anonymize:

```sh
docker exec mongo mongosh --quiet --eval '
  db = db.getSiblingDB("shop");
  db.users.insertMany([
    { _id: 1, email: "alice@corp.com", name: "Alice", age: 34 },
    { _id: 2, email: "bob@corp.com",   name: "Bob",   age: 41 }
  ]);
  db.users.createIndex({ email: 1 }, { unique: true, name: "email_idx" });
'
```

## 2. Write a config

Leafmask is driven by one YAML file. Create `leafmask.yaml`:

```yaml title="leafmask.yaml"
common:
  tmp_dir: ./tmp          # required before a dump runs
  salt: my-stable-seed    # seeds deterministic transformers
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
          name: random_email     # (1)!
        - field: name
          name: random_person    # writes an embedded {first_name, last_name, email}
        - field: age
          name: noise_int
          params:
            ratio: 0.2            # ±20%
```

1.  `random_email` yields **distinct** deterministic addresses, so the unique
    `email_idx` restores cleanly. `masking` would turn every email into stars of
    equal length and violate the unique constraint.

## 3. Preview the transformations

Before producing anything, check the config does what you expect. `validate
--data` runs the transformers over a live sample and shows a before/after diff —
**no dump is written**:

```sh
leafmask --config leafmask.yaml validate --data \
  --database shop --collection users --transformed-only
```

```text
document #0
  * email: "alice@corp.com" -> "user62ca21f3...@example.com"
document #1
  * email: "bob@corp.com" -> "usere1d1cecc...@example.com"
```

!!! tip
    Add `--format json` for machine-readable output, or `--table-format horizontal`
    for a compact layout. See the [`validate`](commands.md#validate) reference.

## 4. Create a dump

```sh
leafmask --config leafmask.yaml dump --include-db shop
```

```text
created dump 20260718T185958Z (428 bytes)
```

The dump landed under `./dumps/<id>/` — one directory per dump, containing the
metadata and the (anonymized, BSON-encoded) collection data.

Inspect what's there:

```sh
leafmask --config leafmask.yaml list-dumps
leafmask --config leafmask.yaml show-dump latest
```

```text
dump 20260718T185958Z
  status: done
  created_at: 2026-07-18T18:59:58+00:00
  size: 428 bytes
  database shop:
    - users (2 docs, 2 indexes) [restore #0]
        index: _id_
        index: email_idx
```

## 5. Restore it

Restore into any target MongoDB (here, back into the same one after wiping the
collection):

```sh
docker exec mongo mongosh --quiet --eval 'db.getSiblingDB("shop").users.drop()'

leafmask --config leafmask.yaml restore latest
```

```text
restored: 2 inserted, 0 skipped, 1 indexes
```

Check the result — the data is back, anonymized, with its unique index:

```sh
docker exec mongo mongosh --quiet --eval '
  db.getSiblingDB("shop").users.find({}, { email: 1 }).toArray()'
```

## Next steps

- Understand the config file: [Configuration overview](configuration/index.md)
- Browse every transformer: [Transformers reference](transformers.md)
- Keep a referentially-consistent subset: [Subsetting](configuration/subsetting.md)
- Full command reference: [Commands](commands.md)
