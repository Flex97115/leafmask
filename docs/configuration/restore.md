# Restore

The `restore` section controls three things: which insert errors to
**tolerate**, which **scripts** to run around the restore, and default
**filtering** lists that command flags can override. Ordering and batching
stay [command flags](../commands.md#restore) only.

```yaml
restore:
  insert_error_exclusions: { ... }
  scripts: { ... }
  include_collections: [ ... ]
  exclude_collections: [ ... ]
  include_indexes: [ ... ]
  exclude_indexes: [ ... ]
  clean: false
```

## Filtering

`include_collections`, `exclude_collections`, `include_indexes`, and
`exclude_indexes` set default filter lists for
[`restore`](../commands.md#restore). A non-empty `--include-collection`,
`--exclude-collection`, `--include-index`, or `--exclude-index` flag on the
command line overrides the matching list entirely (same precedence as
`--uri` overriding `mongodb.uri`) — an unset or empty flag falls back to the
config list.

```yaml
restore:
  include_collections: [users, orders]
  exclude_collections: [sessions]
  include_indexes: [email_idx]
  exclude_indexes: [legacy_idx]
```

## Clean

`clean` drops each restored collection before recreating it from the dump,
same as [`--clean`](../commands.md#restore) on the command line. Either one
turns it on:

```yaml
restore:
  clean: true
```

## Tolerating insert errors

Documents are inserted in bulk batches (`--batch-size`, default 1000 — one
server round-trip per batch, which is what makes restoring very large
collections practical). By default a failed insert fails its **collection**:
the collection is skipped from that point, the restore continues with the next
one, and the command exits non-zero listing the failed collections. Pass
[`--exit-on-error`](../commands.md#restore) to abort the whole restore at the
first non-excluded error instead. Sometimes even that is too strict —
for example a duplicate key you expect and want to skip. Declare exclusions by
**error code** or **unique-index name**, globally or per collection:

```yaml
restore:
  insert_error_exclusions:
    global_error_codes: [11000]        # tolerate duplicate keys everywhere
    global_index_names: [tenant_idx]
    collections:
      - collection: audit_log
        error_codes: [11000]
        index_names: [event_hash_idx]
```

| Field | Description |
| --- | --- |
| `global_error_codes` | error codes tolerated across all collections |
| `global_index_names` | unique-index names tolerated across all collections |
| `collections[].collection` | collection the rule applies to |
| `collections[].error_codes` | codes tolerated for that collection |
| `collections[].index_names` | index names tolerated for that collection |

A matching insert error is **logged and skipped**, and the restore continues with
the next document. An error that matches no exclusion fails the collection (or
the whole restore with `--exit-on-error`). The run's summary reports how many
documents were `skipped`.

!!! tip "Duplicate keys"
    `E11000` is error code `11000`. If you re-restore into a non-empty target, or
    an anonymized field collides on a unique index, adding `11000` to
    `global_error_codes` lets the restore proceed past the collisions.

## Pre/post scripts

Run `mongosh` commands, script files, or external commands at specific stages of
the restore — to prepare or clean up the target database. Scripts are grouped by
stage and run in order; a failing script stops the restore and surfaces the
error.

```yaml
restore:
  scripts:
    pre-data:
      - name: drop target collections
        query: "db.getSiblingDB('shop').users.drop()"      # raw mongosh command
      - name: prepare
        query_file: ./scripts/prepare.js                    # a script file
    post-data:
      - name: reindex
        command: ["./scripts/reindex.sh", "shop"]           # external command
```

Each script sets **exactly one** driver:

| Driver | Runs |
| --- | --- |
| `query` | a raw `mongosh --eval` command |
| `query_file` | a `mongosh` script file |
| `command` | an external command with arguments |

Common stages are `pre-data` (before documents are inserted) and `post-data`
(after). The stage name is just a key, so you can organize scripts however your
workflow needs.

!!! note "mongosh required"
    The `query` and `query_file` drivers invoke `mongosh`; make sure it's on
    `PATH` where you run the restore.
