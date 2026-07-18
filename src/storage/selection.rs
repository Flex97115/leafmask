//! Pluggable storage backend selection (feature `storage.backend-selection`).
//!
//! One entry point turns the `storage` config section into an `Arc<dyn Storage>`.
//! Every command uses the returned trait object and never learns which physical
//! backend is active. An unknown `storage.type` fails fast; a cloud backend that
//! was not compiled in fails with an actionable message.

use std::sync::Arc;

use crate::config::StorageConfig;
use crate::error::{Error, Result};

use super::{DirectoryStorage, Storage};

/// Select and open the storage backend named by `storage.type`.
pub fn open_from_config(cfg: &StorageConfig) -> Result<Arc<dyn Storage>> {
    let kind = cfg.kind.as_deref().ok_or_else(|| {
        Error::Storage("storage.type is required (directory, s3, azure, or ssh)".into())
    })?;
    match kind {
        "directory" => {
            let path = cfg
                .params
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| Error::Storage("directory storage requires a 'path'".into()))?;
            Ok(Arc::new(DirectoryStorage::new(path)?))
        }
        "s3" => open_s3(cfg),
        "azure" => open_azure(cfg),
        "ssh" => open_ssh(cfg),
        other => Err(Error::Storage(format!(
            "unsupported storage.type '{other}' (valid: directory, s3, azure, ssh)"
        ))),
    }
}

#[cfg(feature = "s3")]
fn open_s3(cfg: &StorageConfig) -> Result<Arc<dyn Storage>> {
    let c = super::s3::S3Config::from_params(&cfg.params)?;
    Ok(Arc::new(super::s3::S3Storage::open(c)?))
}
#[cfg(not(feature = "s3"))]
fn open_s3(_cfg: &StorageConfig) -> Result<Arc<dyn Storage>> {
    Err(Error::Storage(
        "storage backend 's3' is not compiled in; rebuild leafmask with --features s3".into(),
    ))
}

#[cfg(feature = "azure")]
fn open_azure(cfg: &StorageConfig) -> Result<Arc<dyn Storage>> {
    let c = super::azure::AzureConfig::from_params(&cfg.params)?;
    Ok(Arc::new(super::azure::AzureStorage::open(c)?))
}
#[cfg(not(feature = "azure"))]
fn open_azure(_cfg: &StorageConfig) -> Result<Arc<dyn Storage>> {
    Err(Error::Storage(
        "storage backend 'azure' is not compiled in; rebuild leafmask with --features azure".into(),
    ))
}

#[cfg(feature = "ssh")]
fn open_ssh(cfg: &StorageConfig) -> Result<Arc<dyn Storage>> {
    let c = super::ssh::SshConfig::from_params(&cfg.params)?;
    Ok(Arc::new(super::ssh::SshStorage::open(c)?))
}
#[cfg(not(feature = "ssh"))]
fn open_ssh(_cfg: &StorageConfig) -> Result<Arc<dyn Storage>> {
    Err(Error::Storage(
        "storage backend 'ssh' is not compiled in; rebuild leafmask with --features ssh".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn storage_cfg(yaml: &str) -> StorageConfig {
        serde_yaml::from_str::<Config>(yaml).unwrap().storage
    }

    // `Arc<dyn Storage>` is not Debug, so `unwrap_err` cannot format it; extract
    // the error by hand.
    fn expect_err(cfg: &StorageConfig) -> Error {
        match open_from_config(cfg) {
            Err(e) => e,
            Ok(_) => panic!("expected an error selecting the backend"),
        }
    }

    // Acceptance: type=directory selects a working backend used uniformly
    // through the trait object.
    #[test]
    fn selects_directory_backend() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = storage_cfg(&format!(
            "storage:\n  type: directory\n  path: {}\n",
            dir.path().display()
        ));
        let s = open_from_config(&cfg).unwrap();
        s.put("d/x", b"hi").unwrap();
        assert_eq!(s.get("d/x").unwrap(), b"hi");
        assert_eq!(s.list_dumps().unwrap(), vec!["d"]);
    }

    // Acceptance: an unsupported/misspelled type fails fast with a clear error.
    #[test]
    fn unknown_type_fails_fast() {
        let cfg = storage_cfg("storage:\n  type: gdrive\n");
        let err = expect_err(&cfg);
        assert!(
            err.to_string()
                .contains("unsupported storage.type 'gdrive'"),
            "{err}"
        );

        // missing type is also an error, not a silent default.
        let cfg = storage_cfg("storage: {}\n");
        assert!(expect_err(&cfg).to_string().contains("required"));
    }

    // With the cloud feature absent, selecting it fails with an actionable
    // message (and when the feature IS compiled in it selects the real backend,
    // verified by the feature build).
    #[test]
    #[cfg(not(feature = "s3"))]
    fn cloud_backend_without_feature_is_clear() {
        let cfg = storage_cfg("storage:\n  type: s3\n  bucket: b\n");
        let err = expect_err(&cfg);
        assert!(err.to_string().contains("--features s3"), "{err}");
    }
}
