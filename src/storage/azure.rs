//! Azure Blob storage backend (feature `storage.azure-backend`).
//!
//! Config parsing and blob-name layout are pure and always tested; the Azure
//! SDK network I/O is behind the `azure` cargo feature (needs a live account —
//! see regeneration-gaps.md).

use crate::config::Params;
use crate::error::{Error, Result};

use super::s3::join_key;

/// Settings for the Azure Blob backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AzureConfig {
    pub account: Option<String>,
    pub container: String,
    /// Access key or SAS token (optional; may come from the environment).
    pub access_key: Option<String>,
    /// Blob-name prefix all dumps live under within the container.
    pub prefix: String,
}

impl AzureConfig {
    pub fn from_params(p: &Params) -> Result<AzureConfig> {
        let container = str_param(p, "container")
            .ok_or_else(|| Error::Storage("azure storage requires a 'container'".into()))?;
        Ok(AzureConfig {
            account: str_param(p, "account"),
            container,
            access_key: str_param(p, "access_key"),
            prefix: str_param(p, "prefix").unwrap_or_default(),
        })
    }

    /// The full blob name for a storage-relative `path`.
    pub fn blob_name(&self, path: &str) -> String {
        join_key(&self.prefix, path)
    }
}

fn str_param(p: &Params, key: &str) -> Option<String> {
    p.get(key).and_then(|v| v.as_str()).map(str::to_string)
}

#[cfg(feature = "azure")]
pub use imp::AzureStorage;

#[cfg(feature = "azure")]
mod imp {
    use super::*;
    use crate::storage::Storage;
    use azure_storage::prelude::*;
    use azure_storage_blobs::prelude::*;
    use futures::StreamExt;
    use tokio::runtime::Runtime;

    /// Live Azure Blob backend.
    pub struct AzureStorage {
        cfg: AzureConfig,
        container: ContainerClient,
        rt: Runtime,
    }

    impl AzureStorage {
        pub fn open(cfg: AzureConfig) -> Result<Self> {
            let account = cfg
                .account
                .clone()
                .ok_or_else(|| Error::Storage("azure storage requires an 'account'".into()))?;
            let key = cfg
                .access_key
                .clone()
                .ok_or_else(|| Error::Storage("azure storage requires an 'access_key'".into()))?;
            let creds = StorageCredentials::access_key(account.clone(), key);
            let container =
                ClientBuilder::new(account, creds).container_client(cfg.container.clone());
            let rt = Runtime::new().map_err(|e| Error::Storage(e.to_string()))?;
            Ok(AzureStorage { cfg, container, rt })
        }
    }

    impl Storage for AzureStorage {
        fn list_dumps(&self) -> Result<Vec<String>> {
            let all = self.list("")?;
            let mut ids: Vec<String> = all
                .into_iter()
                .filter_map(|p| p.split('/').next().map(str::to_string))
                .collect();
            ids.sort();
            ids.dedup();
            Ok(ids)
        }

        fn get(&self, path: &str) -> Result<Vec<u8>> {
            let blob = self.container.blob_client(self.cfg.blob_name(path));
            self.rt
                .block_on(blob.get_content())
                .map_err(|_| Error::NotFound(format!("blob '{path}' not found")))
        }

        fn put(&self, path: &str, data: &[u8]) -> Result<()> {
            let blob = self.container.blob_client(self.cfg.blob_name(path));
            self.rt
                .block_on(blob.put_block_blob(data.to_vec()).into_future())
                .map(|_| ())
                .map_err(|e| Error::Storage(e.to_string()))
        }

        fn exists(&self, path: &str) -> Result<bool> {
            let blob = self.container.blob_client(self.cfg.blob_name(path));
            self.rt
                .block_on(blob.exists())
                .map_err(|e| Error::Storage(e.to_string()))
        }

        fn delete(&self, prefix: &str) -> Result<()> {
            for rel in self.list(prefix)? {
                let blob = self.container.blob_client(self.cfg.blob_name(&rel));
                self.rt
                    .block_on(blob.delete().into_future())
                    .map(|_| ())
                    .map_err(|e| Error::Storage(e.to_string()))?;
            }
            Ok(())
        }

        fn list(&self, prefix: &str) -> Result<Vec<String>> {
            let full = self.cfg.blob_name(prefix);
            let strip = join_key(&self.cfg.prefix, "");
            let mut stream = self.container.list_blobs().prefix(full).into_stream();
            let mut out = Vec::new();
            self.rt.block_on(async {
                while let Some(page) = stream.next().await {
                    let page = page.map_err(|e| Error::Storage(e.to_string()))?;
                    for blob in page.blobs.blobs() {
                        let name = blob.name.strip_prefix(&strip).unwrap_or(&blob.name);
                        out.push(name.to_string());
                    }
                }
                Ok::<(), Error>(())
            })?;
            out.sort();
            Ok(out)
        }

        fn size(&self, prefix: &str) -> Result<u64> {
            let mut total = 0u64;
            for rel in self.list(prefix)? {
                total += self.get(&rel)?.len() as u64;
            }
            Ok(total)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(src: &str) -> Params {
        serde_yaml::from_str(src).unwrap()
    }

    // Acceptance: dumps are stored as blobs under the configured container.
    #[test]
    fn parses_container_and_builds_blob_names() {
        let cfg = AzureConfig::from_params(&params(
            "account: acct\ncontainer: backups\nprefix: leafmask\n",
        ))
        .unwrap();
        assert_eq!(cfg.container, "backups");
        assert_eq!(
            cfg.blob_name("dump-1/metadata.json"),
            "leafmask/dump-1/metadata.json"
        );
    }

    #[test]
    fn requires_container() {
        assert!(AzureConfig::from_params(&params("account: acct\n")).is_err());
    }
}
