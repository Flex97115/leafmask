//! S3-compatible storage backend (feature `storage.s3-backend`).
//!
//! The configuration parsing and object-key layout are pure and always
//! compiled/tested. The network I/O uses the AWS SDK and is compiled only when
//! the `s3` cargo feature is enabled (it requires live S3 credentials to
//! exercise — see regeneration-gaps.md).

use crate::config::Params;
use crate::error::{Error, Result};

/// Settings for the S3 backend, parsed from the `storage` config section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct S3Config {
    pub bucket: String,
    pub region: Option<String>,
    /// Explicit endpoint override for S3-compatible services (e.g. MinIO, GCS).
    pub endpoint: Option<String>,
    /// Key prefix all dumps live under within the bucket.
    pub prefix: String,
    pub access_key_id: Option<String>,
    pub secret_access_key: Option<String>,
}

impl S3Config {
    /// Parse from raw storage params. `bucket` is required.
    pub fn from_params(p: &Params) -> Result<S3Config> {
        let bucket = str_param(p, "bucket")
            .ok_or_else(|| Error::Storage("s3 storage requires a 'bucket'".into()))?;
        Ok(S3Config {
            bucket,
            region: str_param(p, "region"),
            endpoint: str_param(p, "endpoint"),
            prefix: str_param(p, "prefix").unwrap_or_default(),
            access_key_id: str_param(p, "access_key_id"),
            secret_access_key: str_param(p, "secret_access_key"),
        })
    }

    /// The full object key for a storage-relative `path`.
    pub fn object_key(&self, path: &str) -> String {
        join_key(&self.prefix, path)
    }
}

fn str_param(p: &Params, key: &str) -> Option<String> {
    p.get(key).and_then(|v| v.as_str()).map(str::to_string)
}

/// Join a key prefix and a relative path with a single `/`, tolerating leading
/// and trailing slashes on either side.
pub fn join_key(prefix: &str, path: &str) -> String {
    let p = prefix.trim_matches('/');
    let path = path.trim_start_matches('/');
    if p.is_empty() {
        path.to_string()
    } else {
        format!("{p}/{path}")
    }
}

#[cfg(feature = "s3")]
pub use imp::S3Storage;

#[cfg(feature = "s3")]
mod imp {
    use super::*;
    use crate::storage::Storage;
    use aws_sdk_s3::primitives::ByteStream;
    use aws_sdk_s3::Client;
    use tokio::runtime::Runtime;

    /// Live S3 backend. Holds its own multi-thread runtime so the synchronous
    /// [`Storage`] trait can drive the async SDK.
    pub struct S3Storage {
        cfg: S3Config,
        client: Client,
        rt: Runtime,
    }

    impl S3Storage {
        pub fn open(cfg: S3Config) -> Result<Self> {
            let rt = Runtime::new().map_err(|e| Error::Storage(e.to_string()))?;
            let client = rt.block_on(async {
                let mut loader = aws_config::defaults(aws_config::BehaviorVersion::latest());
                if let Some(r) = &cfg.region {
                    loader = loader.region(aws_config::Region::new(r.clone()));
                }
                if let Some(e) = &cfg.endpoint {
                    loader = loader.endpoint_url(e.clone());
                }
                let shared = loader.load().await;
                Client::new(&shared)
            });
            Ok(S3Storage { cfg, client, rt })
        }
    }

    impl Storage for S3Storage {
        fn list_dumps(&self) -> Result<Vec<String>> {
            let prefix = join_key(&self.cfg.prefix, "");
            let resp = self
                .rt
                .block_on(
                    self.client
                        .list_objects_v2()
                        .bucket(&self.cfg.bucket)
                        .prefix(&prefix)
                        .delimiter("/")
                        .send(),
                )
                .map_err(|e| Error::Storage(e.to_string()))?;
            let mut ids: Vec<String> = resp
                .common_prefixes()
                .iter()
                .filter_map(|cp| cp.prefix())
                .filter_map(|p| {
                    p.trim_end_matches('/')
                        .rsplit('/')
                        .next()
                        .map(str::to_string)
                })
                .collect();
            ids.sort();
            Ok(ids)
        }

        fn get(&self, path: &str) -> Result<Vec<u8>> {
            let key = self.cfg.object_key(path);
            let resp = self
                .rt
                .block_on(self.client.get_object().bucket(&self.cfg.bucket).key(&key).send())
                .map_err(|_| Error::NotFound(format!("blob '{path}' not found")))?;
            let bytes = self
                .rt
                .block_on(resp.body.collect())
                .map_err(|e| Error::Storage(e.to_string()))?;
            Ok(bytes.into_bytes().to_vec())
        }

        fn put(&self, path: &str, data: &[u8]) -> Result<()> {
            let key = self.cfg.object_key(path);
            self.rt
                .block_on(
                    self.client
                        .put_object()
                        .bucket(&self.cfg.bucket)
                        .key(&key)
                        .body(ByteStream::from(data.to_vec()))
                        .send(),
                )
                .map_err(|e| Error::Storage(e.to_string()))?;
            Ok(())
        }

        fn exists(&self, path: &str) -> Result<bool> {
            let key = self.cfg.object_key(path);
            Ok(self
                .rt
                .block_on(self.client.head_object().bucket(&self.cfg.bucket).key(&key).send())
                .is_ok())
        }

        fn delete(&self, prefix: &str) -> Result<()> {
            for rel in self.list(prefix)? {
                let key = self.cfg.object_key(&rel);
                self.rt
                    .block_on(self.client.delete_object().bucket(&self.cfg.bucket).key(&key).send())
                    .map_err(|e| Error::Storage(e.to_string()))?;
            }
            Ok(())
        }

        fn list(&self, prefix: &str) -> Result<Vec<String>> {
            let full = self.cfg.object_key(prefix);
            let resp = self
                .rt
                .block_on(
                    self.client
                        .list_objects_v2()
                        .bucket(&self.cfg.bucket)
                        .prefix(&full)
                        .send(),
                )
                .map_err(|e| Error::Storage(e.to_string()))?;
            let strip = join_key(&self.cfg.prefix, "");
            let mut out: Vec<String> = resp
                .contents()
                .iter()
                .filter_map(|o| o.key())
                .map(|k| k.strip_prefix(&strip).unwrap_or(k).to_string())
                .collect();
            out.sort();
            Ok(out)
        }

        fn size(&self, prefix: &str) -> Result<u64> {
            let full = self.cfg.object_key(prefix);
            let resp = self
                .rt
                .block_on(
                    self.client
                        .list_objects_v2()
                        .bucket(&self.cfg.bucket)
                        .prefix(&full)
                        .send(),
                )
                .map_err(|e| Error::Storage(e.to_string()))?;
            Ok(resp.contents().iter().map(|o| o.size().unwrap_or(0).max(0) as u64).sum())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(src: &str) -> Params {
        serde_yaml::from_str(src).unwrap()
    }

    // Acceptance: dumps upload under the configured bucket and key prefix.
    #[test]
    fn parses_bucket_and_prefix_and_builds_keys() {
        let cfg = S3Config::from_params(&params("bucket: dumps\nprefix: leafmask\n")).unwrap();
        assert_eq!(cfg.bucket, "dumps");
        assert_eq!(cfg.object_key("dump-1/metadata.json"), "leafmask/dump-1/metadata.json");

        // no prefix -> key is the path itself.
        let cfg = S3Config::from_params(&params("bucket: b\n")).unwrap();
        assert_eq!(cfg.object_key("d/x"), "d/x");
    }

    // Acceptance: S3-compatible endpoints (e.g. MinIO) via explicit override.
    #[test]
    fn supports_endpoint_override() {
        let cfg = S3Config::from_params(&params(
            "bucket: b\nregion: us-east-1\nendpoint: http://minio:9000\n",
        ))
        .unwrap();
        assert_eq!(cfg.endpoint.as_deref(), Some("http://minio:9000"));
        assert_eq!(cfg.region.as_deref(), Some("us-east-1"));
    }

    // Missing bucket fails fast.
    #[test]
    fn requires_bucket() {
        assert!(S3Config::from_params(&params("region: us-east-1\n")).is_err());
    }

    #[test]
    fn join_key_normalizes_slashes() {
        assert_eq!(join_key("/pre/", "/a/b"), "pre/a/b");
        assert_eq!(join_key("", "a/b"), "a/b");
    }
}
