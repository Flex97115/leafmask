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
    /// Point at an Azurite-compatible emulator instead of the public Azure
    /// cloud. When set, `account`/`access_key` are ignored — the emulator's
    /// well-known development credentials are used instead.
    pub emulator_host: Option<String>,
    /// Defaults to Azurite's standard blob-service port (10000) when
    /// `emulator_host` is set but this is not.
    pub emulator_port: Option<u16>,
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
            emulator_host: str_param(p, "emulator_host"),
            emulator_port: p
                .get("emulator_port")
                .and_then(|v| v.as_u64())
                .map(|v| v as u16),
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
    use crate::storage::multipart::{PartSink, StreamingMultipartWriter};
    use crate::storage::{MultipartWriter, Storage};
    use azure_storage::prelude::*;
    use azure_storage::CloudLocation;
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
            let rt = Runtime::new().map_err(|e| Error::Storage(e.to_string()))?;
            let container = if let Some(host) = cfg.emulator_host.clone() {
                let port = cfg.emulator_port.unwrap_or(10000);
                ClientBuilder::with_location(
                    CloudLocation::Emulator {
                        address: host,
                        port,
                    },
                    StorageCredentials::emulator(),
                )
                .container_client(cfg.container.clone())
            } else {
                let account = cfg
                    .account
                    .clone()
                    .ok_or_else(|| Error::Storage("azure storage requires an 'account'".into()))?;
                let key = cfg
                    .access_key
                    .clone()
                    .ok_or_else(|| Error::Storage("azure storage requires an 'access_key'".into()))?;
                let creds = StorageCredentials::access_key(account.clone(), key);
                ClientBuilder::new(account, creds).container_client(cfg.container.clone())
            };
            Ok(AzureStorage { cfg, container, rt })
        }
    }

    struct AzurePartSink {
        handle: tokio::runtime::Handle,
        blob: BlobClient,
        blob_name: String,
    }

    impl PartSink for AzurePartSink {
        fn send_part(&mut self, part_number: u64, data: Vec<u8>) -> Result<String> {
            log::info!(
                "  sending block {part_number} ({:.1} MiB) to azure: {}",
                data.len() as f64 / (1024.0 * 1024.0),
                self.blob_name
            );
            // Azure requires every block ID for a blob to be the same byte
            // length before base64 encoding; zero-padding the part number
            // to a fixed width guarantees that regardless of magnitude.
            let id = format!("{part_number:020}");
            self.handle
                .block_on(
                    self.blob
                        .put_block(BlockId::new(id.clone().into_bytes()), data)
                        .into_future(),
                )
                .map_err(|e| Error::Storage(format!("put_block: {e}")))?;
            Ok(id)
        }

        fn complete(self: Box<Self>, tokens: Vec<String>) -> Result<()> {
            let blocks = tokens
                .into_iter()
                .map(|id| BlobBlockType::new_latest(BlockId::new(id.into_bytes())))
                .collect();
            self.handle
                .block_on(self.blob.put_block_list(BlockList { blocks }).into_future())
                .map_err(|e| Error::Storage(format!("put_block_list: {e}")))?;
            Ok(())
        }

        fn abort(&mut self) {
            // Azure has no explicit "abort" for uncommitted blocks: a block
            // that's never referenced by put_block_list is simply garbage
            // collected by the service after 7 days. Nothing to do here.
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
            // The listing already carries each blob's content length — sizing
            // a dump must not download it.
            let full = self.cfg.blob_name(prefix);
            let mut stream = self.container.list_blobs().prefix(full).into_stream();
            let mut total = 0u64;
            self.rt.block_on(async {
                while let Some(page) = stream.next().await {
                    let page = page.map_err(|e| Error::Storage(e.to_string()))?;
                    for blob in page.blobs.blobs() {
                        total += blob.properties.content_length;
                    }
                }
                Ok::<(), Error>(())
            })?;
            Ok(total)
        }

        fn multipart_writer(&self, path: &str) -> Result<Option<Box<dyn MultipartWriter>>> {
            let blob_name = self.cfg.blob_name(path);
            let blob = self.container.blob_client(blob_name.clone());
            let sink = AzurePartSink {
                handle: self.rt.handle().clone(),
                blob,
                blob_name,
            };
            Ok(Some(Box::new(StreamingMultipartWriter::spawn(Box::new(sink)))))
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

    // An emulator host lets leafmask target Azurite (or a self-hosted
    // Azurite-compatible endpoint) instead of the public Azure cloud; the
    // port defaults to Azurite's standard blob-service port when omitted.
    #[test]
    fn parses_emulator_host_and_port() {
        let cfg = AzureConfig::from_params(&params(
            "container: backups\nemulator_host: 127.0.0.1\nemulator_port: 12345\n",
        ))
        .unwrap();
        assert_eq!(cfg.emulator_host.as_deref(), Some("127.0.0.1"));
        assert_eq!(cfg.emulator_port, Some(12345));

        // No emulator_host set -> None, unaffected (existing production path).
        let cfg = AzureConfig::from_params(&params("account: acct\ncontainer: backups\n")).unwrap();
        assert_eq!(cfg.emulator_host, None);
    }
}
