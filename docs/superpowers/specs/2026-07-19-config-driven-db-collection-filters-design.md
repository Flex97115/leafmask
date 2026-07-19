# Config-driven include/exclude filters for dump and restore

## Problem

`dump` and `restore` accept repeatable filter flags:

- `dump`: `--include-db`, `--exclude-db`, `--include-collection`, `--exclude-collection`
- `restore`: `--include-collection`, `--exclude-collection`, `--include-index`, `--exclude-index`

When a deployment has many databases/collections, the CLI invocation becomes
unwieldy. These lists should be settable in the YAML config file instead,
while still allowing the CLI flags to work standalone (no config) or to
override the config for a one-off run.

## Design

### New config fields

Added to the existing `dump:` and `restore:` sections (both currently parsed
as loose `serde_yaml::Value` and refined locally, e.g. `subset_conds` in
`src/cli.rs`). Field names match the internal `DumpOptions` /
`RestoreOptions` struct field names for a direct mental mapping:

```yaml
dump:
  include_databases: [shop, billing]
  exclude_databases: [logs]
  include_collections: [users, orders]
  exclude_collections: [sessions]

restore:
  include_collections: [users, orders]
  exclude_collections: [sessions]
  include_indexes: [email_idx]
  exclude_indexes: [legacy_idx]
```

All fields are optional and default to an empty list.

### Precedence

A **non-empty CLI flag overrides the corresponding YAML list entirely**; an
empty/unset CLI flag falls back to the YAML list. This mirrors the existing
`--uri` vs `mongodb.uri` override behavior, applied per-list instead of
per-scalar. Lists are not merged/unioned — this keeps the resolved value
predictable (exactly the CLI list, or exactly the YAML list, never a
combination a user didn't write down in one place).

### Implementation shape

- `src/cli.rs`:
  - Extend the existing local `DumpSection` struct (used today for
    `transformation` and `subset_conds`) with the four new dump fields, and
    the existing local `RestoreSection` struct (used today for
    `insert_error_exclusions` and `scripts`) with the four new restore
    fields.
  - Add a small helper:
    ```rust
    fn resolve_list(cli: Vec<String>, config: Vec<String>) -> Vec<String> {
        if cli.is_empty() { config } else { cli }
    }
    ```
    used for all 8 list pairs (4 in `cmd_dump`, 4 in `cmd_restore`).
  - No change to `DumpOptions`, `RestoreOptions`, `src/dump/create.rs`, or
    `src/restore/database.rs` — the merge happens before those option structs
    are constructed, so the filtering logic itself is untouched.

### Docs

- `docs/commands.md`: for each of the 8 flags, note the YAML field it falls
  back to when the flag is omitted.
- `docs/configuration/restore.md`: currently states "Filtering, ordering, and
  batching are command flags" — update this, and add a short "Filtering"
  subsection documenting the four `restore:` fields and the override rule.
- Add an equivalent short section for `dump:`'s four fields. There is no
  dedicated `docs/configuration/dump.md` today (dump config lives split
  across `transformations.md` and `subsetting.md`, both about different,
  document-level mechanisms) — rather than introduce a new page for four
  fields, add the section directly to `docs/commands.md` next to the `dump`
  flag table.

### Tests

- Unit tests for `resolve_list` in `src/cli.rs`: empty CLI list falls back to
  config; non-empty CLI list wins outright (config list, even if non-empty,
  is discarded).
- `src/config.rs`-level (or `cli.rs`-level) test that the new YAML fields
  deserialize correctly under `dump:` and `restore:`, including the
  already-enforced "unknown key" rejection still working for genuinely
  unknown keys.
- At least one `tests/mongo_integration.rs` case exercising a YAML-only
  filter (no CLI flags) for dump, confirming the config list actually drives
  which collections/databases get dumped.

## Out of scope

- Merging/union semantics (explicitly rejected in favor of override).
- Any change to filtering logic itself (`DumpOptions::include_db` /
  `RestoreOptions::include_collection` etc. are unchanged).
- `validate`'s `--database`/`--collection` (single required values, not
  repeatable filter lists — not part of this request).
