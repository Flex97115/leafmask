//! Dump management: list, show, and delete/prune dumps
//! (features `dumpmgmt.list-dumps`, `dumpmgmt.show-dump`, `dumpmgmt.delete-dump`).

use std::collections::HashSet;

use chrono::{DateTime, Duration, Utc};

use crate::error::Result;
use crate::storage::Storage;

use super::{list_metadata, read_metadata, resolve, DumpMetadata, DumpStatus};

/// A dump as surfaced by `list-dumps`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DumpListing {
    pub id: String,
    pub status: DumpStatus,
    pub size: u64,
}

/// List every dump in storage. An empty storage yields an empty list, not an
/// error.
pub fn list_entries(storage: &dyn Storage) -> Result<Vec<DumpListing>> {
    let mut out: Vec<DumpListing> = list_metadata(storage)?
        .into_iter()
        .map(|m| DumpListing {
            id: m.id,
            status: m.status,
            size: m.size,
        })
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

/// Render the listing as lines of `id  status  size`.
pub fn list_dumps(storage: &dyn Storage) -> Result<String> {
    Ok(list_entries(storage)?
        .into_iter()
        .map(|d| format!("{}\t{}\t{} bytes", d.id, d.status.as_str(), d.size))
        .collect::<Vec<_>>()
        .join("\n"))
}

/// Render one dump's table of contents / metadata (`show-dump`). Accepts a
/// literal id or `latest`; a nonexistent id is a clear error.
pub fn show_dump(storage: &dyn Storage, id_or_latest: &str) -> Result<String> {
    Ok(resolve(storage, id_or_latest)?.render_toc())
}

/// Delete a single dump by id, and only that dump.
pub fn delete_dump(storage: &dyn Storage, id: &str) -> Result<()> {
    // Confirm it exists first so a typo is a clear error, not a silent no-op.
    read_metadata(storage, id)?;
    storage.delete(id)
}

/// A retention policy for pruning multiple dumps at once.
#[derive(Debug, Clone, Default)]
pub struct RetentionPolicy {
    /// Keep only the N most recent completed dumps.
    pub retain_recent: Option<usize>,
    /// Keep dumps created within this duration of `now`.
    pub retain_for: Option<Duration>,
    /// Delete dumps created strictly before this date.
    pub before_date: Option<DateTime<Utc>>,
    /// Delete dumps that did not complete successfully.
    pub prune_failed: bool,
    /// Also delete unknown/in-progress dumps (with `prune_failed`).
    pub prune_unsafe: bool,
}

fn created_at(m: &DumpMetadata) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(&m.created_at)
        .map(|d| d.with_timezone(&Utc))
        .unwrap_or_else(|_| DateTime::<Utc>::from_timestamp(0, 0).unwrap())
}

/// Decide which dump ids a policy would delete, given the current dumps and
/// `now`. Pure and deterministic so it is fully unit-testable.
pub fn select_for_deletion(
    dumps: &[DumpMetadata],
    policy: &RetentionPolicy,
    now: DateTime<Utc>,
) -> Vec<String> {
    // The N most recent completed dumps are protected from every rule.
    let protected: HashSet<String> = match policy.retain_recent {
        Some(n) => {
            let mut done: Vec<&DumpMetadata> = dumps
                .iter()
                .filter(|m| m.status == DumpStatus::Done)
                .collect();
            done.sort_by_key(|d| std::cmp::Reverse(created_at(d)));
            done.into_iter().take(n).map(|m| m.id.clone()).collect()
        }
        None => HashSet::new(),
    };

    let mut del = Vec::new();
    for m in dumps {
        if protected.contains(&m.id) {
            continue;
        }
        let created = created_at(m);
        let mut candidate = false;

        if policy.retain_recent.is_some() && m.status == DumpStatus::Done {
            // completed dumps not in the protected set are excess.
            candidate = true;
        }
        if let Some(dur) = policy.retain_for {
            if created < now - dur {
                candidate = true;
            }
        }
        if let Some(bd) = policy.before_date {
            if created < bd {
                candidate = true;
            }
        }
        if policy.prune_failed && m.status == DumpStatus::Failed {
            candidate = true;
        }
        if policy.prune_unsafe && matches!(m.status, DumpStatus::Unknown | DumpStatus::InProgress) {
            candidate = true;
        }

        if candidate {
            del.push(m.id.clone());
        }
    }
    del.sort();
    del
}

/// Apply a retention policy. With `dry_run`, reports the ids that would be
/// deleted without touching storage; otherwise deletes them.
pub fn prune(
    storage: &dyn Storage,
    policy: &RetentionPolicy,
    now: DateTime<Utc>,
    dry_run: bool,
) -> Result<Vec<String>> {
    let dumps = list_metadata(storage)?;
    let to_delete = select_for_deletion(&dumps, policy, now);
    if !dry_run {
        for id in &to_delete {
            storage.delete(id)?;
        }
    }
    Ok(to_delete)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dump::{write_metadata, CollectionToc, DatabaseToc};
    use crate::storage::{DirectoryStorage, Storage};

    fn meta(id: &str, status: DumpStatus, created: &str) -> DumpMetadata {
        DumpMetadata {
            id: id.to_string(),
            status,
            created_at: created.to_string(),
            databases: vec![DatabaseToc {
                name: "app".into(),
                collections: vec![CollectionToc {
                    name: "users".into(),
                    document_count: 1,
                    indexes: vec![],
                    restore_order: 0,
                }],
            }],
            size: 10,
        }
    }

    fn store_with(dumps: &[DumpMetadata]) -> (tempfile::TempDir, DirectoryStorage) {
        let dir = tempfile::tempdir().unwrap();
        let s = DirectoryStorage::new(dir.path()).unwrap();
        for m in dumps {
            write_metadata(&s, m).unwrap();
        }
        (dir, s)
    }

    // Acceptance (list): each dump's id and status; empty storage is empty.
    #[test]
    fn lists_ids_and_status_and_empty() {
        let (_d, s) = store_with(&[
            meta("a", DumpStatus::Done, "2026-01-01T00:00:00Z"),
            meta("b", DumpStatus::Failed, "2026-01-02T00:00:00Z"),
        ]);
        let entries = list_entries(&s).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].id, "a");
        assert_eq!(entries[1].status, DumpStatus::Failed);

        let empty_dir = tempfile::tempdir().unwrap();
        let empty = DirectoryStorage::new(empty_dir.path()).unwrap();
        assert!(list_entries(&empty).unwrap().is_empty());
        assert_eq!(list_dumps(&empty).unwrap(), "");
    }

    // Acceptance (show): existing id prints TOC; latest resolves; missing errors.
    #[test]
    fn shows_toc_latest_and_missing() {
        let (_d, s) = store_with(&[
            meta("old", DumpStatus::Done, "2026-01-01T00:00:00Z"),
            meta("new", DumpStatus::Done, "2026-02-01T00:00:00Z"),
        ]);
        assert!(show_dump(&s, "old").unwrap().contains("dump old"));
        assert!(show_dump(&s, "latest").unwrap().contains("dump new"));
        assert!(show_dump(&s, "ghost").is_err());
    }

    // Acceptance (delete): deleting by id removes only that dump.
    #[test]
    fn deletes_only_the_named_dump() {
        let (_d, s) = store_with(&[
            meta("a", DumpStatus::Done, "2026-01-01T00:00:00Z"),
            meta("b", DumpStatus::Done, "2026-01-02T00:00:00Z"),
        ]);
        delete_dump(&s, "a").unwrap();
        assert_eq!(s.list_dumps().unwrap(), vec!["b"]);
        // deleting a missing dump is a clear error.
        assert!(delete_dump(&s, "a").is_err());
    }

    // Acceptance (delete): retain-recent keeps N most recent, deletes the rest.
    #[test]
    fn retain_recent_keeps_n_most_recent() {
        let dumps = vec![
            meta("d1", DumpStatus::Done, "2026-01-01T00:00:00Z"),
            meta("d2", DumpStatus::Done, "2026-01-02T00:00:00Z"),
            meta("d3", DumpStatus::Done, "2026-01-03T00:00:00Z"),
        ];
        let policy = RetentionPolicy {
            retain_recent: Some(2),
            ..Default::default()
        };
        let del = select_for_deletion(&dumps, &policy, Utc::now());
        assert_eq!(del, vec!["d1".to_string()]);
    }

    // Foot-gun guard: `retain_recent: 0` protects zero dumps, so every completed
    // dump is selected for deletion. This documents the (literal) "keep 0"
    // semantics so the total-wipe behaviour cannot regress unnoticed.
    #[test]
    fn retain_recent_zero_selects_every_completed_dump() {
        let dumps = vec![
            meta("d1", DumpStatus::Done, "2026-01-01T00:00:00Z"),
            meta("d2", DumpStatus::Done, "2026-01-02T00:00:00Z"),
            meta("d3", DumpStatus::Done, "2026-01-03T00:00:00Z"),
        ];
        let policy = RetentionPolicy {
            retain_recent: Some(0),
            ..Default::default()
        };
        let del = select_for_deletion(&dumps, &policy, Utc::now());
        assert_eq!(
            del,
            vec!["d1".to_string(), "d2".to_string(), "d3".to_string()]
        );
    }

    // Acceptance (delete): before-date / retain-for delete outside the window;
    // prune-failed removes failed dumps; dry-run deletes nothing.
    #[test]
    fn date_window_prune_failed_and_dry_run() {
        let (_d, s) = store_with(&[
            meta("old", DumpStatus::Done, "2020-01-01T00:00:00Z"),
            meta("fresh", DumpStatus::Done, "2026-07-01T00:00:00Z"),
            meta("broken", DumpStatus::Failed, "2026-07-02T00:00:00Z"),
        ]);
        let now = DateTime::parse_from_rfc3339("2026-07-18T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        // before_date drops the 2020 dump only.
        let by_date = RetentionPolicy {
            before_date: Some(
                DateTime::parse_from_rfc3339("2021-01-01T00:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
            ),
            ..Default::default()
        };
        assert_eq!(
            prune(&s, &by_date, now, true).unwrap(),
            vec!["old".to_string()]
        );
        // dry-run kept everything.
        assert_eq!(list_entries(&s).unwrap().len(), 3);

        // prune_failed drops only the failed dump.
        let failed = RetentionPolicy {
            prune_failed: true,
            ..Default::default()
        };
        let deleted = prune(&s, &failed, now, false).unwrap();
        assert_eq!(deleted, vec!["broken".to_string()]);
        assert_eq!(s.list_dumps().unwrap(), vec!["fresh", "old"]);
    }
}
