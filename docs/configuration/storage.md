# Storage backends

Dumps are read and written through a single storage abstraction. Pick the
physical backend with `storage.type`; every command (`dump`, `restore`,
`list-dumps`, `show-dump`, `delete`) then works identically regardless of which
one is active.

```yaml
storage:
  type: directory   # directory | s3 | azure | ssh
  # ...backend-specific settings...
```

!!! warning "Fail fast"
    An unknown or misspelled `storage.type` is a startup error — Leafmask never
    silently falls back to a default.

!!! info "Feature-gated backends"
    Only **directory** is always available. `s3`, `azure`, and `ssh` are compiled
    in via [Cargo features](../installation.md#feature-matrix) (all present in the
    prebuilt binaries and Docker image). Selecting a backend that wasn't compiled
    in prints a clear "rebuild with `--features …`" error.

Each dump is stored under its own id (a directory / key prefix), so a single
backend can hold many dumps side by side.

## Directory

Stores dumps on the local filesystem, one subdirectory per dump id.

```yaml
storage:
  type: directory
  path: ./dumps       # (1)!
```

1.  Created if it doesn't exist. Use an absolute path in production.

| Field | Required | Description |
| --- | --- | --- |
| `path` | yes | base directory for all dumps |

## S3

Any S3-compatible object store — AWS S3, MinIO, GCS, etc.

```yaml
storage:
  type: s3
  bucket: my-leafmask-dumps
  region: eu-west-1
  prefix: production        # optional key prefix
  access_key_id: ${AWS_ACCESS_KEY_ID}
  secret_access_key: ${AWS_SECRET_ACCESS_KEY}
```

| Field | Required | Description |
| --- | --- | --- |
| `bucket` | yes | target bucket |
| `region` | no | AWS region |
| `endpoint` | no | override for S3-compatible services (MinIO, GCS, …) |
| `prefix` | no | key prefix all dumps live under |
| `access_key_id` | no | falls back to the standard AWS credential chain |
| `secret_access_key` | no | falls back to the standard AWS credential chain |
| `force_path_style` | no | `true`/`false` — path-style addressing. Defaults to `true` when `endpoint` is set (what MinIO and most S3-compatibles need), `false` otherwise. |

=== "AWS S3"

    ```yaml
    storage:
      type: s3
      bucket: acme-dumps
      region: us-east-1
    ```

=== "MinIO / custom endpoint"

    ```yaml
    storage:
      type: s3
      bucket: dumps
      region: us-east-1
      endpoint: http://minio:9000
      access_key_id: ${MINIO_ROOT_USER}
      secret_access_key: ${MINIO_ROOT_PASSWORD}
    ```

    Path-style addressing is automatic when `endpoint` is set — MinIO and most S3-compatibles require it. Config credentials (`access_key_id`/`secret_access_key`) take precedence over the standard AWS credential chain when present.

## Azure Blob

```yaml
storage:
  type: azure
  account: mystorageaccount
  container: leafmask-dumps
  access_key: ${AZURE_STORAGE_KEY}
  prefix: production        # optional blob-name prefix
```

| Field | Required | Description |
| --- | --- | --- |
| `container` | yes | target blob container |
| `account` | no* | storage account name (*required by the live backend) |
| `access_key` | no* | account key or SAS token |
| `prefix` | no | blob-name prefix all dumps live under |

## SSH / SFTP

Stores dumps on a remote server over SFTP. The connection is established lazily
on first use, not on every command startup.

```yaml
storage:
  type: ssh
  host: backup.example.com
  port: 22
  username: dumper
  private_key: ~/.ssh/id_ed25519   # or use `password:`
  base_path: /srv/leafmask/dumps
```

| Field | Required | Description |
| --- | --- | --- |
| `host` | yes | remote host |
| `port` | no | SSH port (default `22`) |
| `username` | no | login user |
| `password` | no | password authentication |
| `private_key` | no | path to a private key for key-based auth |
| `base_path` | no | remote base directory (default `.`) |
