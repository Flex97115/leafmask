//! Dump domain: the on-storage dump metadata / table-of-contents format shared
//! by every command that reads or writes dumps, plus dump creation and dump
//! management (list/show/delete).

pub mod management;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::storage::Storage;

/// Completion status of a dump.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DumpStatus {
    /// Completed successfully.
    Done,
    /// Started but did not finish.
    Failed,
    /// Currently running.
    InProgress,
    /// Status could not be determined.
    Unknown,
}

impl DumpStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            DumpStatus::Done => "done",
            DumpStatus::Failed => "failed",
            DumpStatus::InProgress => "in-progress",
            DumpStatus::Unknown => "unknown",
        }
    }
}

/// One collection's entry in the table of contents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionToc {
    pub name: String,
    #[serde(default)]
    pub document_count: u64,
    #[serde(default)]
    pub indexes: Vec<String>,
    /// Position in the restore order (documents before indexes/validators).
    #[serde(default)]
    pub restore_order: u32,
}

/// One database's entry in the table of contents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatabaseToc {
    pub name: String,
    #[serde(default)]
    pub collections: Vec<CollectionToc>,
}

/// A dump's metadata document, stored at `<id>/metadata.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DumpMetadata {
    pub id: String,
    pub status: DumpStatus,
    /// RFC3339 creation timestamp; the dump id is derived from it.
    pub created_at: String,
    #[serde(default)]
    pub databases: Vec<DatabaseToc>,
    #[serde(default)]
    pub size: u64,
}

impl DumpMetadata {
    /// The storage path of a dump's metadata file.
    pub fn metadata_path(id: &str) -> String {
        format!("{id}/metadata.json")
    }

    /// A readable table-of-contents rendering for `show-dump`.
    pub fn render_toc(&self) -> String {
        let mut out = format!(
            "dump {}\n  status: {}\n  created_at: {}\n  size: {} bytes\n",
            self.id,
            self.status.as_str(),
            self.created_at,
            self.size
        );
        for db in &self.databases {
            out.push_str(&format!("  database {}:\n", db.name));
            let mut collections = db.collections.clone();
            collections.sort_by_key(|c| c.restore_order);
            for c in &collections {
                out.push_str(&format!(
                    "    - {} ({} docs, {} indexes) [restore #{}]\n",
                    c.name,
                    c.document_count,
                    c.indexes.len(),
                    c.restore_order
                ));
                for idx in &c.indexes {
                    out.push_str(&format!("        index: {idx}\n"));
                }
            }
        }
        out
    }
}

/// Persist a dump's metadata to storage.
pub fn write_metadata(storage: &dyn Storage, meta: &DumpMetadata) -> Result<()> {
    let json = serde_json::to_vec_pretty(meta).map_err(|e| Error::Storage(e.to_string()))?;
    storage.put(&DumpMetadata::metadata_path(&meta.id), &json)
}

/// Read one dump's metadata from storage. Errors with `NotFound` if the dump or
/// its metadata is absent.
pub fn read_metadata(storage: &dyn Storage, id: &str) -> Result<DumpMetadata> {
    let bytes = storage.get(&DumpMetadata::metadata_path(id)).map_err(|e| match e {
        Error::NotFound(_) => Error::NotFound(format!("dump '{id}' not found")),
        other => other,
    })?;
    serde_json::from_slice(&bytes).map_err(|e| Error::Storage(format!("corrupt metadata for '{id}': {e}")))
}

/// Read metadata for every dump present in storage. Dumps whose metadata cannot
/// be read are surfaced as `Unknown`-status stubs rather than failing the whole
/// listing.
pub fn list_metadata(storage: &dyn Storage) -> Result<Vec<DumpMetadata>> {
    let mut out = Vec::new();
    for id in storage.list_dumps()? {
        match read_metadata(storage, &id) {
            Ok(meta) => out.push(meta),
            Err(_) => out.push(DumpMetadata {
                id: id.clone(),
                status: DumpStatus::Unknown,
                created_at: String::new(),
                databases: Vec::new(),
                size: 0,
            }),
        }
    }
    Ok(out)
}

/// Resolve a dump reference: a literal id, or `latest` -> the most recently
/// created `Done` dump.
pub fn resolve(storage: &dyn Storage, id_or_latest: &str) -> Result<DumpMetadata> {
    if id_or_latest == "latest" {
        let mut done: Vec<DumpMetadata> = list_metadata(storage)?
            .into_iter()
            .filter(|m| m.status == DumpStatus::Done)
            .collect();
        done.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        done.into_iter()
            .next()
            .ok_or_else(|| Error::NotFound("no completed dump found for 'latest'".into()))
    } else {
        read_metadata(storage, id_or_latest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::DirectoryStorage;

    fn meta(id: &str, status: DumpStatus, created: &str) -> DumpMetadata {
        DumpMetadata {
            id: id.to_string(),
            status,
            created_at: created.to_string(),
            databases: vec![DatabaseToc {
                name: "app".into(),
                collections: vec![CollectionToc {
                    name: "users".into(),
                    document_count: 5,
                    indexes: vec!["email_idx".into()],
                    restore_order: 0,
                }],
            }],
            size: 100,
        }
    }

    #[test]
    fn writes_reads_and_resolves() {
        let dir = tempfile::tempdir().unwrap();
        let s = DirectoryStorage::new(dir.path()).unwrap();
        write_metadata(&s, &meta("20260101T000000Z", DumpStatus::Done, "2026-01-01T00:00:00Z")).unwrap();
        write_metadata(&s, &meta("20260102T000000Z", DumpStatus::Done, "2026-01-02T00:00:00Z")).unwrap();

        let read = read_metadata(&s, "20260101T000000Z").unwrap();
        assert_eq!(read.status, DumpStatus::Done);
        assert!(read.render_toc().contains("email_idx"));

        // latest -> most recent Done.
        assert_eq!(resolve(&s, "latest").unwrap().id, "20260102T000000Z");
        // nonexistent -> clear error.
        assert!(matches!(read_metadata(&s, "nope"), Err(Error::NotFound(_))));
    }
}
