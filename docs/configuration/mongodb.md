# MongoDB connection

The `mongodb` section tells Leafmask how to reach the deployment that `dump`,
`restore`, and `validate --data` operate on.

```yaml
mongodb:
  uri: mongodb://localhost:27017
```

That's the only field. It accepts any standard [MongoDB connection
string](https://www.mongodb.com/docs/manual/reference/connection-string/),
including credentials, options, replica sets, and SRV records.

## Examples

=== "Local"

    ```yaml
    mongodb:
      uri: mongodb://localhost:27017
    ```

=== "With auth"

    ```yaml
    mongodb:
      uri: mongodb://appuser:${MONGO_PASSWORD}@db.internal:27017/?authSource=admin
    ```

=== "Replica set"

    ```yaml
    mongodb:
      uri: mongodb://host1:27017,host2:27017,host3:27017/?replicaSet=rs0
    ```

=== "Atlas (SRV)"

    ```yaml
    mongodb:
      uri: mongodb+srv://user:${ATLAS_PASSWORD}@cluster0.abcde.mongodb.net/
    ```

!!! tip "Keep credentials out of the file"
    Use [`${ENV}` interpolation](index.md#environment-interpolation) for
    passwords and connection strings.

## Overriding at the command line

`--uri` (or the `LEAFMASK_MONGO_URI` environment variable) overrides
`mongodb.uri` for a single run:

```sh
leafmask --config leafmask.yaml --uri mongodb://staging:27017 dump
```

## Access model

Leafmask has **no user system of its own** — it authenticates to MongoDB with the
credentials in the URI, and to storage with the backend's credentials. Whatever
those accounts can read and write is exactly what Leafmask can do. Grant the
dump account read access to the source, and the restore account write access to
the target.

!!! note "Consistency"
    Reads use a plain query. Point-in-time snapshot consistency across a dump
    requires a **replica set** (a standalone `mongod` does not support snapshot
    sessions).
