# Config-driven include/exclude filters for dump and restore — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let `dump`'s `--include-db`/`--exclude-db`/`--include-collection`/`--exclude-collection` and `restore`'s `--include-collection`/`--exclude-collection`/`--include-index`/`--exclude-index` be set in the YAML config file, with a non-empty CLI flag overriding the config value.

**Architecture:** Two new pure-parsing functions in `src/cli.rs` (`dump_filter_lists`, `restore_filter_lists`) read the new config fields out of the existing `config.dump` / `config.restore` raw `serde_yaml::Value`, following the exact pattern already used by `transformation_configs` (one function, one private local struct, one `serde_yaml::from_value` call). A new `resolve_list` helper applies the override rule. Both are wired into `cmd_dump`/`cmd_restore` right before the existing `DumpOptions`/`RestoreOptions` construction — no changes to `DumpOptions`, `RestoreOptions`, `src/dump/create.rs`, or `src/restore/database.rs`.

**Tech Stack:** Rust, clap, serde/serde_yaml (already in use in `src/cli.rs`).

## Global Constraints

- CLI flag wins only when non-empty; empty/unset CLI flag falls back to the config list (no merging/union) — confirmed design decision.
- No change to the filtering logic itself (`DumpOptions`/`RestoreOptions` and their `include_*`/`exclude_*` methods are untouched).
- Follow the existing local-struct-per-concern pattern in `src/cli.rs` (see `transformation_configs`) rather than growing an existing multi-field struct.
- New config fields default to empty lists; omitting them entirely (or omitting `dump:`/`restore:` altogether) must not error.
- At the end of all tasks (Task 6), delete both this plan file and its spec (`docs/superpowers/specs/2026-07-19-config-driven-db-collection-filters-design.md`) from `docs/` — that folder is Leafmask's published MkDocs site, not a scratch space for planning docs (explicit user instruction).

---

## File Structure

- Modify: `src/cli.rs` — add `resolve_list`, `dump_filter_lists`, `restore_filter_lists`, wire them into `cmd_dump`/`cmd_restore`, add a `#[cfg(test)] mod tests` block at the end of the file (none exists today).
- Modify: `docs/commands.md` — document the YAML fallback for the 8 flags.
- Modify: `docs/configuration/restore.md` — update the "filtering is CLI-only" line, add a "Filtering" section.
- Modify: `tests/mongo_integration.rs` — one end-to-end test proving a config-only (`dump.include_databases`, no `--include-db`) run restricts the dump correctly.

---

### Task 1: `resolve_list` helper

**Files:**
- Modify: `src/cli.rs` (insert after `resolve_uri`, which currently ends at line 179, i.e. immediately before the `transformation_configs` doc comment on line 181)
- Test: `src/cli.rs` (new `#[cfg(test)] mod tests` block appended at end of file, line 528 today)

**Interfaces:**
- Produces: `fn resolve_list(cli: Vec<String>, config: Vec<String>) -> Vec<String>` — non-empty `cli` wins outright; empty `cli` returns `config` unchanged. Used by Task 2 and Task 3.

- [ ] **Step 1: Write the failing tests**

Append this module at the very end of `src/cli.rs` (after the closing `}` of `cmd_validate`, currently line 528):

```rust

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_list_falls_back_to_config_when_cli_is_empty() {
        let result = resolve_list(vec![], vec!["a".to_string(), "b".to_string()]);
        assert_eq!(result, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn resolve_list_cli_overrides_nonempty_config_entirely() {
        let result = resolve_list(
            vec!["x".to_string()],
            vec!["a".to_string(), "b".to_string()],
        );
        assert_eq!(result, vec!["x".to_string()]);
    }

    #[test]
    fn resolve_list_both_empty_is_empty() {
        let result: Vec<String> = resolve_list(vec![], vec![]);
        assert!(result.is_empty());
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail to compile**

Run: `cargo test --lib cli::tests -- --nocapture`
Expected: FAIL — `error[E0425]: cannot find function `resolve_list` in this scope`

- [ ] **Step 3: Implement `resolve_list`**

Insert immediately after `resolve_uri` (i.e. right before the `/// Parse the `dump.transformation` section...` doc comment currently at line 181):

```rust
/// Resolve a repeatable filter list: a non-empty CLI value overrides the
/// config value entirely (same precedence as `--uri` over `mongodb.uri`,
/// applied per-list instead of per-scalar). An empty CLI value falls back to
/// whatever the config declared, including empty.
#[cfg_attr(not(feature = "mongo"), allow(dead_code))]
fn resolve_list(cli: Vec<String>, config: Vec<String>) -> Vec<String> {
    if cli.is_empty() {
        config
    } else {
        cli
    }
}

```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib cli::tests -- --nocapture`
Expected: PASS (3 passed)

- [ ] **Step 5: Commit**

```bash
git add src/cli.rs
git commit -m "feat(cli): add resolve_list helper for CLI-overrides-config filter lists"
```

---

### Task 2: `dump_filter_lists` config parsing, wired into `cmd_dump`

**Files:**
- Modify: `src/cli.rs`
  - Insert new function after `transformation_configs` / before `pub fn run` (i.e. right after the closing `}` of `transformation_configs`, currently line 210)
  - Modify `cmd_dump` (currently lines 328–366)
  - Test: append to the `mod tests` block added in Task 1

**Interfaces:**
- Consumes: `resolve_list(cli: Vec<String>, config: Vec<String>) -> Vec<String>` from Task 1; `crate::config::Config` (existing); `crate::config::parse_str(&str) -> crate::Result<Config>` (existing, `src/config.rs`).
- Produces: `fn dump_filter_lists(config: &crate::config::Config) -> crate::Result<(Vec<String>, Vec<String>, Vec<String>, Vec<String>)>` returning `(include_databases, exclude_databases, include_collections, exclude_collections)`. Used by Task 4/5 docs and by `cmd_dump`.

- [ ] **Step 1: Write the failing tests**

Append inside the `mod tests` block (after the Task 1 tests, before the closing `}` of `mod tests`):

```rust

    #[test]
    fn dump_filter_lists_parses_all_four_fields() {
        let config = crate::config::parse_str(
            "dump:\n  include_databases: [shop]\n  exclude_databases: [logs]\n  include_collections: [users]\n  exclude_collections: [sessions]\n",
        )
        .unwrap();
        let (include_db, exclude_db, include_coll, exclude_coll) =
            dump_filter_lists(&config).unwrap();
        assert_eq!(include_db, vec!["shop".to_string()]);
        assert_eq!(exclude_db, vec!["logs".to_string()]);
        assert_eq!(include_coll, vec!["users".to_string()]);
        assert_eq!(exclude_coll, vec!["sessions".to_string()]);
    }

    #[test]
    fn dump_filter_lists_defaults_to_empty_without_dump_section() {
        let config = crate::config::parse_str("common:\n  tmp_dir: /tmp\n").unwrap();
        let (include_db, exclude_db, include_coll, exclude_coll) =
            dump_filter_lists(&config).unwrap();
        assert!(include_db.is_empty());
        assert!(exclude_db.is_empty());
        assert!(include_coll.is_empty());
        assert!(exclude_coll.is_empty());
    }

    #[test]
    fn dump_filter_lists_ignores_unrelated_dump_fields() {
        let config = crate::config::parse_str(
            "dump:\n  transformation: []\n  include_databases: [shop]\n",
        )
        .unwrap();
        let (include_db, _, _, _) = dump_filter_lists(&config).unwrap();
        assert_eq!(include_db, vec!["shop".to_string()]);
    }
```

- [ ] **Step 2: Run the tests to verify they fail to compile**

Run: `cargo test --lib cli::tests -- --nocapture`
Expected: FAIL — `error[E0425]: cannot find function `dump_filter_lists` in this scope`

- [ ] **Step 3: Implement `dump_filter_lists`**

Insert right after `transformation_configs`'s closing `}` (currently line 210), before the `/// Entry point invoked by `main`.` doc comment on `pub fn run`:

```rust

/// Parse `dump.include_databases` / `exclude_databases` / `include_collections`
/// / `exclude_collections` from config. These are the YAML fallback for the
/// `--include-db`/`--exclude-db`/`--include-collection`/`--exclude-collection`
/// flags — see `resolve_list` for the override precedence.
#[cfg_attr(not(feature = "mongo"), allow(dead_code))]
fn dump_filter_lists(
    config: &crate::config::Config,
) -> crate::Result<(Vec<String>, Vec<String>, Vec<String>, Vec<String>)> {
    if config.dump.is_null() {
        return Ok(Default::default());
    }
    #[derive(serde::Deserialize, Default)]
    struct DumpSection {
        #[serde(default)]
        include_databases: Vec<String>,
        #[serde(default)]
        exclude_databases: Vec<String>,
        #[serde(default)]
        include_collections: Vec<String>,
        #[serde(default)]
        exclude_collections: Vec<String>,
    }
    let section: DumpSection = serde_yaml::from_value(config.dump.clone())
        .map_err(|e| crate::Error::Config(format!("dump filters: {e}")))?;
    Ok((
        section.include_databases,
        section.exclude_databases,
        section.include_collections,
        section.exclude_collections,
    ))
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib cli::tests -- --nocapture`
Expected: PASS (6 passed — 3 from Task 1 + 3 new)

- [ ] **Step 5: Wire `dump_filter_lists` + `resolve_list` into `cmd_dump`**

In `cmd_dump` (currently lines 328–366), the `let options = DumpOptions { ... }` block (currently lines 341–350) changes from:

```rust
    let options = DumpOptions {
        tmp_dir: config.common.tmp_dir.clone(),
        include_databases: args.include_db.clone(),
        exclude_databases: args.exclude_db.clone(),
        include_collections: args.include_collection.clone(),
        exclude_collections: args.exclude_collection.clone(),
        gzip: args.gzip,
        parallel_jobs: args.jobs,
        no_indexes: args.no_indexes,
    };
```

to:

```rust
    let (cfg_include_db, cfg_exclude_db, cfg_include_coll, cfg_exclude_coll) =
        dump_filter_lists(&config)?;
    let options = DumpOptions {
        tmp_dir: config.common.tmp_dir.clone(),
        include_databases: resolve_list(args.include_db.clone(), cfg_include_db),
        exclude_databases: resolve_list(args.exclude_db.clone(), cfg_exclude_db),
        include_collections: resolve_list(args.include_collection.clone(), cfg_include_coll),
        exclude_collections: resolve_list(args.exclude_collection.clone(), cfg_exclude_coll),
        gzip: args.gzip,
        parallel_jobs: args.jobs,
        no_indexes: args.no_indexes,
    };
```

- [ ] **Step 6: Confirm the crate builds with the `mongo` feature**

Run: `cargo build --features mongo`
Expected: builds with no errors (a live MongoDB is not required to build)

- [ ] **Step 7: Run the full test suite**

Run: `cargo test --lib`
Expected: PASS, no regressions

- [ ] **Step 8: Commit**

```bash
git add src/cli.rs
git commit -m "feat(dump): allow include/exclude db and collection filters in config"
```

---

### Task 3: `restore_filter_lists` config parsing, wired into `cmd_restore`

**Files:**
- Modify: `src/cli.rs`
  - Insert new function directly after `dump_filter_lists` (added by Task 2, right after `transformation_configs`), before `pub fn run`, for locality with the other filter-list parser.
  - Modify `cmd_restore` (currently lines 403–444, after Task 2's edits — Task 2 inserted `dump_filter_lists` earlier in the file, shifting `cmd_restore` down; match by code content, not line number, since exact numbers will have shifted again once your own new lines are added)
  - Test: append to `mod tests`

**Interfaces:**
- Consumes: `resolve_list` (Task 1).
- Produces: `fn restore_filter_lists(config: &crate::config::Config) -> crate::Result<(Vec<String>, Vec<String>, Vec<String>, Vec<String>)>` returning `(include_collections, exclude_collections, include_indexes, exclude_indexes)`.

- [ ] **Step 1: Write the failing tests**

Append inside `mod tests`, after the Task 2 tests:

```rust

    #[test]
    fn restore_filter_lists_parses_all_four_fields() {
        let config = crate::config::parse_str(
            "restore:\n  include_collections: [users]\n  exclude_collections: [sessions]\n  include_indexes: [email_idx]\n  exclude_indexes: [legacy_idx]\n",
        )
        .unwrap();
        let (include_coll, exclude_coll, include_idx, exclude_idx) =
            restore_filter_lists(&config).unwrap();
        assert_eq!(include_coll, vec!["users".to_string()]);
        assert_eq!(exclude_coll, vec!["sessions".to_string()]);
        assert_eq!(include_idx, vec!["email_idx".to_string()]);
        assert_eq!(exclude_idx, vec!["legacy_idx".to_string()]);
    }

    #[test]
    fn restore_filter_lists_defaults_to_empty_without_restore_section() {
        let config = crate::config::parse_str("common:\n  tmp_dir: /tmp\n").unwrap();
        let (include_coll, exclude_coll, include_idx, exclude_idx) =
            restore_filter_lists(&config).unwrap();
        assert!(include_coll.is_empty());
        assert!(exclude_coll.is_empty());
        assert!(include_idx.is_empty());
        assert!(exclude_idx.is_empty());
    }

    #[test]
    fn restore_filter_lists_ignores_unrelated_restore_fields() {
        let config = crate::config::parse_str(
            "restore:\n  insert_error_exclusions:\n    global_error_codes: [11000]\n  include_collections: [users]\n",
        )
        .unwrap();
        let (include_coll, _, _, _) = restore_filter_lists(&config).unwrap();
        assert_eq!(include_coll, vec!["users".to_string()]);
    }
```

- [ ] **Step 2: Run the tests to verify they fail to compile**

Run: `cargo test --lib cli::tests -- --nocapture`
Expected: FAIL — `error[E0425]: cannot find function `restore_filter_lists` in this scope`

- [ ] **Step 3: Implement `restore_filter_lists`**

Insert right after `dump_filter_lists`'s closing `}` (added by Task 2, immediately after `transformation_configs`), before the `/// Entry point invoked by `main`.` doc comment on `pub fn run`:

```rust

/// Parse `restore.include_collections` / `exclude_collections` /
/// `include_indexes` / `exclude_indexes` from config. These are the YAML
/// fallback for the `--include-collection`/`--exclude-collection`/
/// `--include-index`/`--exclude-index` flags — see `resolve_list` for the
/// override precedence. Parsed separately from the `insert_error_exclusions`
/// / `scripts` section already handled inside `cmd_restore`, mirroring how
/// `dump.transformation` and `dump.subset_conds` are each parsed by their own
/// function above.
#[cfg_attr(not(feature = "mongo"), allow(dead_code))]
fn restore_filter_lists(
    config: &crate::config::Config,
) -> crate::Result<(Vec<String>, Vec<String>, Vec<String>, Vec<String>)> {
    if config.restore.is_null() {
        return Ok(Default::default());
    }
    #[derive(serde::Deserialize, Default)]
    struct RestoreFilterSection {
        #[serde(default)]
        include_collections: Vec<String>,
        #[serde(default)]
        exclude_collections: Vec<String>,
        #[serde(default)]
        include_indexes: Vec<String>,
        #[serde(default)]
        exclude_indexes: Vec<String>,
    }
    let section: RestoreFilterSection = serde_yaml::from_value(config.restore.clone())
        .map_err(|e| crate::Error::Config(format!("restore filters: {e}")))?;
    Ok((
        section.include_collections,
        section.exclude_collections,
        section.include_indexes,
        section.exclude_indexes,
    ))
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib cli::tests -- --nocapture`
Expected: PASS (9 passed — 6 from Tasks 1–2 + 3 new)

- [ ] **Step 5: Wire `restore_filter_lists` + `resolve_list` into `cmd_restore`**

In `cmd_restore` (currently lines 403–444), the `let options = RestoreOptions { ... };` block (currently lines 425–434) changes from:

```rust
    let options = RestoreOptions {
        include_collections: args.include_collection.clone(),
        exclude_collections: args.exclude_collection.clone(),
        include_indexes: args.include_index.clone(),
        exclude_indexes: args.exclude_index.clone(),
        batch_size: args.batch_size,
        ordered: args.ordered,
        dependency_order: args.dependency_order,
        exit_on_error: args.exit_on_error,
    };
```

to:

```rust
    let (cfg_include_coll, cfg_exclude_coll, cfg_include_idx, cfg_exclude_idx) =
        restore_filter_lists(&config)?;
    let options = RestoreOptions {
        include_collections: resolve_list(args.include_collection.clone(), cfg_include_coll),
        exclude_collections: resolve_list(args.exclude_collection.clone(), cfg_exclude_coll),
        include_indexes: resolve_list(args.include_index.clone(), cfg_include_idx),
        exclude_indexes: resolve_list(args.exclude_index.clone(), cfg_exclude_idx),
        batch_size: args.batch_size,
        ordered: args.ordered,
        dependency_order: args.dependency_order,
        exit_on_error: args.exit_on_error,
    };
```

This must be inserted **before** the existing `let section: RestoreSection = ...` block is consumed by the later `Restore { ... exclusions: section.insert_error_exclusions, scripts: section.scripts, ... }` — placing the new `let (cfg_include_coll, ...)` line immediately above `let options = RestoreOptions { ... }` (i.e. right after the existing `let section: RestoreSection = if config.restore.is_null() { ... };` block, currently ending line 423) keeps that ordering intact; no other lines in `cmd_restore` move.

- [ ] **Step 6: Confirm the crate builds with the `mongo` feature**

Run: `cargo build --features mongo`
Expected: builds with no errors

- [ ] **Step 7: Run the full test suite**

Run: `cargo test --lib`
Expected: PASS, no regressions

- [ ] **Step 8: Commit**

```bash
git add src/cli.rs
git commit -m "feat(restore): allow include/exclude collection and index filters in config"
```

---

### Task 4: Documentation

**Files:**
- Modify: `docs/commands.md`
- Modify: `docs/configuration/restore.md`

**Interfaces:**
- Consumes: nothing (docs only; no build interface). Describes the config field names fixed in Task 2/3: `dump.include_databases`, `dump.exclude_databases`, `dump.include_collections`, `dump.exclude_collections`, `restore.include_collections`, `restore.exclude_collections`, `restore.include_indexes`, `restore.exclude_indexes`.

- [ ] **Step 1: Update the `dump` section of `docs/commands.md`**

The current flag table and surrounding text (lines 62–73) is:

```markdown
| Flag | Description |
| --- | --- |
| `--gzip` | compress collection data |
| `--include-db <name>` | restrict to these databases (repeatable) |
| `--exclude-db <name>` | skip these databases (repeatable) |
| `--include-collection <name>` | restrict to these collections (repeatable) |
| `--exclude-collection <name>` | skip these collections (repeatable) |
| `--no-indexes` | exclude index definitions and collection options |
| `--jobs <n>` | number of parallel jobs (accepted; runs sequentially) |

Requires `common.tmp_dir`; without it the dump fails fast. Prints the new dump
id.
```

Replace it with:

```markdown
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
```

- [ ] **Step 2: Update the `restore` section of `docs/commands.md`**

The current flag table and surrounding text (lines 99–113) is:

```markdown
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

Tolerated insert errors and pre/post scripts come from the
[`restore`](configuration/restore.md) config section. Prints a summary
(`inserted`, `skipped`, `indexes`).
```

Replace it with:

```markdown
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
```

- [ ] **Step 3: Update `docs/configuration/restore.md`**

The current intro (lines 1–12) is:

```markdown
# Restore

The `restore` section controls two things that happen when you restore a dump:
which insert errors to **tolerate**, and which **scripts** to run around the
restore. Filtering, ordering, and batching are [command
flags](../commands.md#restore).

```yaml
restore:
  insert_error_exclusions: { ... }
  scripts: { ... }
```
```

Replace it with:

```markdown
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
```
```

Then, immediately before the `## Tolerating insert errors` heading, insert a new section:

```markdown
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

```

- [ ] **Step 4: Commit**

```bash
git add docs/commands.md docs/configuration/restore.md
git commit -m "docs: document config-driven dump/restore filter lists"
```

---

### Task 5: End-to-end integration test

**Files:**
- Modify: `tests/mongo_integration.rs`

**Interfaces:**
- Consumes: `leafmask::cli::{run, Cli, Command, DumpArgs}` (all `pub`, confirmed in `src/cli.rs` and re-exported via `pub mod cli;` in `src/lib.rs`); `leafmask::storage::DirectoryStorage::new` (existing, already imported in this file); `leafmask::dump::management::list_metadata` (existing `pub fn` in `src/dump/management.rs`, not currently imported in this file).

This test proves the wiring from Tasks 2–3 end-to-end: a `dump.include_databases` config entry, with **no** `--include-db` flag, actually restricts what gets dumped — the scenario the user asked for (avoiding a long CLI invocation).

- [ ] **Step 1: Add the `list_metadata` import**

In `tests/mongo_integration.rs`, the current imports (lines 14–23) include:

```rust
use leafmask::dump::{read_collection_full, Dump, DumpOptions};
```

Add a new import line directly after it:

```rust
use leafmask::dump::management::list_metadata;
```

- [ ] **Step 2: Write the test**

Append at the end of `tests/mongo_integration.rs`:

```rust

// A `dump.include_databases` config entry, with no `--include-db` flag on
// the command line, is enough to restrict which database gets dumped — the
// scenario that motivated moving these filters into config (avoids spelling
// out every database/collection on the CLI).
#[test]
fn dump_filters_from_config_include_databases_without_cli_flag() {
    let m = connect();
    let wanted = db_name("cfgfilt_keep");
    let skipped = db_name("cfgfilt_skip");
    m.ensure_collection(&wanted, "users", &None, &BTreeMap::new())
        .unwrap();
    m.insert(&wanted, "users", &user(1, "a@x.com")).unwrap();
    m.ensure_collection(&skipped, "users", &None, &BTreeMap::new())
        .unwrap();
    m.insert(&skipped, "users", &user(2, "b@x.com")).unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let storage_path = tmp.path().join("store");
    let work_dir = tmp.path().join("work");
    std::fs::create_dir_all(&work_dir).unwrap();
    let config_path = tmp.path().join("leafmask.yaml");
    std::fs::write(
        &config_path,
        format!(
            "common:\n  tmp_dir: {}\nstorage:\n  type: directory\n  path: {}\ndump:\n  include_databases: [{}]\n",
            work_dir.display(),
            storage_path.display(),
            wanted,
        ),
    )
    .unwrap();

    let cli = leafmask::cli::Cli {
        config: Some(config_path),
        uri: Some(uri()),
        command: leafmask::cli::Command::Dump(leafmask::cli::DumpArgs {
            gzip: false,
            include_db: vec![],
            exclude_db: vec![],
            include_collection: vec![],
            exclude_collection: vec![],
            jobs: 1,
            no_indexes: false,
        }),
    };
    leafmask::cli::run(cli).unwrap();

    let storage = leafmask::storage::DirectoryStorage::new(&storage_path).unwrap();
    let dumps = list_metadata(&storage).unwrap();
    assert_eq!(dumps.len(), 1);
    let db_names: Vec<&str> = dumps[0]
        .databases
        .iter()
        .map(|d| d.name.as_str())
        .collect();
    assert_eq!(db_names, vec![wanted.as_str()]);

    m.drop_database(&wanted).unwrap();
    m.drop_database(&skipped).unwrap();
}
```

- [ ] **Step 3: Run it against a live MongoDB**

Run:
```sh
docker run -d --name leafmask-mongo-test -p 27017:27017 mongo:7
cargo test --features mongo --test mongo_integration dump_filters_from_config_include_databases_without_cli_flag -- --nocapture
docker rm -f leafmask-mongo-test
```
Expected: PASS. If Docker/MongoDB is unavailable in this environment, state that explicitly rather than claiming the test passed — this is the one step in the plan that needs a live MongoDB, everything else (Tasks 1–4) does not.

- [ ] **Step 4: Run the full integration suite to confirm no regressions**

Run: `cargo test --features mongo --test mongo_integration`
Expected: PASS (all tests, including the pre-existing ones)

- [ ] **Step 5: Commit**

```bash
git add tests/mongo_integration.rs
git commit -m "test: cover config-only dump.include_databases end-to-end"
```

---

### Task 6: Clean up planning docs from `docs/`

**Files:**
- Delete: `docs/superpowers/specs/2026-07-19-config-driven-db-collection-filters-design.md`
- Delete: `docs/superpowers/plans/2026-07-19-config-driven-db-collection-filters.md` (this file)

`docs/` is Leafmask's published MkDocs site (see `mkdocs.yml`, `edit_uri: edit/main/docs/`), not a scratch space — these two files must not ship in it once the feature is done (explicit user instruction).

- [ ] **Step 1: Confirm Tasks 1–5 are all committed and clean**

Run: `cargo fmt --check && cargo clippy --all-targets --features mongo -- -D warnings && cargo test --lib && cargo build --features mongo`
Expected: PASS / builds clean, no fmt diffs, no clippy warnings (this repo's CI is clippy-`-D warnings`-clean per its commit history — run this before the final commit, not just `cargo test`)

- [ ] **Step 2: Remove the planning docs**

```bash
git rm docs/superpowers/specs/2026-07-19-config-driven-db-collection-filters-design.md
git rm docs/superpowers/plans/2026-07-19-config-driven-db-collection-filters.md
```

- [ ] **Step 3: Commit**

```bash
git commit -m "chore: remove planning docs for config-driven filters from docs/"
```
