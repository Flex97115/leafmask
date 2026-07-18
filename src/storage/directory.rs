//! Local filesystem storage backend (feature `storage.directory-backend`).

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

use super::Storage;

/// Stores dumps as directories under a configured root path, one subdirectory
/// per dump ID.
#[derive(Debug, Clone)]
pub struct DirectoryStorage {
    root: PathBuf,
}

impl DirectoryStorage {
    /// Open (creating if necessary) a directory-backed storage at `root`.
    pub fn new(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        fs::create_dir_all(&root)
            .map_err(|e| Error::Storage(format!("cannot create {}: {e}", root.display())))?;
        Ok(DirectoryStorage { root })
    }

    fn resolve(&self, rel: &str) -> PathBuf {
        let mut p = self.root.clone();
        for seg in rel.split('/').filter(|s| !s.is_empty() && *s != ".") {
            p.push(seg);
        }
        p
    }
}

/// Recursively collect file paths under `dir`, as `/`-joined paths relative to
/// `base`.
fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(dir).map_err(|e| Error::Storage(e.to_string()))? {
        let entry = entry.map_err(|e| Error::Storage(e.to_string()))?;
        let path = entry.path();
        let ft = entry.file_type().map_err(|e| Error::Storage(e.to_string()))?;
        if ft.is_dir() {
            walk(base, &path, out)?;
        } else if let Ok(rel) = path.strip_prefix(base) {
            out.push(rel.to_string_lossy().replace(std::path::MAIN_SEPARATOR, "/"));
        }
    }
    Ok(())
}

impl Storage for DirectoryStorage {
    fn list_dumps(&self) -> Result<Vec<String>> {
        let mut ids = Vec::new();
        if !self.root.exists() {
            return Ok(ids);
        }
        for entry in fs::read_dir(&self.root).map_err(|e| Error::Storage(e.to_string()))? {
            let entry = entry.map_err(|e| Error::Storage(e.to_string()))?;
            if entry
                .file_type()
                .map_err(|e| Error::Storage(e.to_string()))?
                .is_dir()
            {
                ids.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
        ids.sort();
        Ok(ids)
    }

    fn get(&self, path: &str) -> Result<Vec<u8>> {
        let full = self.resolve(path);
        match fs::read(&full) {
            Ok(b) => Ok(b),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(Error::NotFound(format!("blob '{path}' not found")))
            }
            Err(e) => Err(Error::Storage(e.to_string())),
        }
    }

    fn put(&self, path: &str, data: &[u8]) -> Result<()> {
        let full = self.resolve(path);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).map_err(|e| Error::Storage(e.to_string()))?;
        }
        fs::write(&full, data).map_err(|e| Error::Storage(e.to_string()))
    }

    fn exists(&self, path: &str) -> Result<bool> {
        Ok(self.resolve(path).exists())
    }

    fn delete(&self, prefix: &str) -> Result<()> {
        let full = self.resolve(prefix);
        if full.is_dir() {
            fs::remove_dir_all(&full).map_err(|e| Error::Storage(e.to_string()))
        } else if full.exists() {
            fs::remove_file(&full).map_err(|e| Error::Storage(e.to_string()))
        } else {
            Ok(())
        }
    }

    fn list(&self, prefix: &str) -> Result<Vec<String>> {
        let mut out = Vec::new();
        walk(&self.root, &self.resolve(prefix), &mut out)?;
        out.sort();
        Ok(out)
    }

    fn size(&self, prefix: &str) -> Result<u64> {
        let mut total = 0u64;
        for rel in self.list(prefix)? {
            let meta = fs::metadata(self.resolve(&rel)).map_err(|e| Error::Storage(e.to_string()))?;
            total += meta.len();
        }
        Ok(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Acceptance: dumps land under the configured dir, one subdir per dump ID.
    #[test]
    fn writes_one_subdir_per_dump_id() {
        let dir = tempfile::tempdir().unwrap();
        let s = DirectoryStorage::new(dir.path()).unwrap();
        s.put("dump-1/metadata.json", b"{}").unwrap();
        s.put("dump-1/data/users.bson", b"rows").unwrap();
        s.put("dump-2/metadata.json", b"{}").unwrap();

        assert!(dir.path().join("dump-1/metadata.json").exists());
        assert_eq!(s.list_dumps().unwrap(), vec!["dump-1", "dump-2"]);
    }

    // Acceptance: restore/list/delete read and modify files under that dir.
    #[test]
    fn reads_lists_and_deletes() {
        let dir = tempfile::tempdir().unwrap();
        let s = DirectoryStorage::new(dir.path()).unwrap();
        s.put("d/metadata.json", b"meta").unwrap();
        s.put("d/data/a.bson", b"aaaa").unwrap();

        assert_eq!(s.get("d/metadata.json").unwrap(), b"meta");
        assert!(s.exists("d/data/a.bson").unwrap());
        assert_eq!(s.list("d").unwrap(), vec!["d/data/a.bson", "d/metadata.json"]);
        assert_eq!(s.size("d").unwrap(), 4 + 4);

        s.delete("d").unwrap();
        assert!(!s.exists("d").unwrap());
        assert_eq!(s.list_dumps().unwrap(), Vec::<String>::new());
    }

    #[test]
    fn missing_blob_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let s = DirectoryStorage::new(dir.path()).unwrap();
        let err = s.get("nope").unwrap_err();
        assert!(matches!(err, Error::NotFound(_)));
        // empty storage lists cleanly.
        assert!(s.list_dumps().unwrap().is_empty());
    }
}
