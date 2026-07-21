# Full config example

A complete, annotated configuration touching every section. Copy it as a
starting point and trim what you don't need.

```yaml title="leafmask.yaml"
# ---------------------------------------------------------------------------
# Global settings
# ---------------------------------------------------------------------------
common:
  tmp_dir: ./tmp              # required before `dump` runs
  salt: ${LEAFMASK_SALT}      # stable seed for deterministic transformers

# ---------------------------------------------------------------------------
# Where to read from / write to
# ---------------------------------------------------------------------------
mongodb:
  uri: ${MONGO_URI}           # e.g. mongodb://user:pass@host:27017/?authSource=admin

storage:
  type: s3
  bucket: acme-leafmask-dumps
  region: eu-west-1
  prefix: production
  access_key_id: ${AWS_ACCESS_KEY_ID}
  secret_access_key: ${AWS_SECRET_ACCESS_KEY}
  # endpoint: http://minio:9000  # override for S3-compatible services (MinIO, GCS, …)

# ---------------------------------------------------------------------------
# Anonymization applied while dumping
# ---------------------------------------------------------------------------
dump:
  transformation:
    - collection: users
      # Only anonymize (and dump) active users.
      query:
        active: true
      transformers:
        - field: _id
          name: random_object_id
          apply_for_references: true      # propagate to every *.user_id

        - field: email
          name: random_email              # distinct -> safe under unique index

        - field: profile
          name: random_person             # embedded {first_name,last_name,email}

        - field: ssn
          name: masking
          params:
            char: "•"

        - field: age
          name: noise_int
          params:
            ratio: 0.15

        - field: marketing_opt_in
          name: set_null
          when: "region == 'EU'"          # GDPR: null it for EU users

    - collection: orders
      transformers:
        - field: card_last4
          name: regexp_replace
          params:
            regexp: "[0-9]"
            replace: "X"

        - field: updated_at
          name: noise_date
          params:
            max_days: 30
          dynamic_params:
            max_days:
              field: retention_days       # bound taken from the document

# ---------------------------------------------------------------------------
# Your own transformers
# ---------------------------------------------------------------------------
custom_transformers:
  - name: order_ref
    driver: template
    template: "ORD-{{ region }}-{{ _id }}"

  - name: external_masker
    driver: command
    command: ["python3", "./scripts/masker.py"]
    parameters:
      - name: locale
        type: string
        required: true

# ---------------------------------------------------------------------------
# Referential subsetting (see the Subsetting page for status)
# ---------------------------------------------------------------------------
subset_conds:
  orders: "total > 100 and region == 'EU'"

virtual_references:
  - collection: orders
    references:
      - field: user_id
        references_collection: users
      - field: coupon_id
        references_collection: coupons
        not_null: false
  - collection: comments
    references:
      - field: parent_id
        polymorphic_exprs:
          - field: parent_type
            value: post
            references_collection: posts
          - field: parent_type
            value: photo
            references_collection: photos

# ---------------------------------------------------------------------------
# Restore behavior
# ---------------------------------------------------------------------------
restore:
  clean: false                            # drop each collection before restoring it
  insert_error_exclusions:
    global_error_codes: [11000]           # tolerate duplicate keys
    collections:
      - collection: audit_log
        index_names: [event_hash_idx]
  scripts:
    pre-data:
      - name: clean target
        query: "db.getSiblingDB('shop').users.drop()"
    post-data:
      - name: warm caches
        command: ["./scripts/warm.sh"]

# ---------------------------------------------------------------------------
# Warnings you've reviewed and want to stop seeing
# ---------------------------------------------------------------------------
resolved_warnings:
  - a1b2c3d4e5f6
```

!!! tip "Validate before you dump"
    Run `leafmask --config leafmask.yaml validate --data --database shop
    --collection users` to preview the transformations against real documents
    first.
