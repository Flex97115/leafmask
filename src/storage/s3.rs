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
    pub force_path_style: Option<bool>,
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
            force_path_style: bool_param(p, "force_path_style"),
        })
    }

    /// The full object key for a storage-relative `path`.
    pub fn object_key(&self, path: &str) -> String {
        join_key(&self.prefix, path)
    }

    /// Whether the SDK should use path-style addressing. Explicit setting wins;
    /// otherwise a custom endpoint implies path-style (MinIO & most compatibles).
    pub fn path_style(&self) -> bool {
        self.force_path_style.unwrap_or(self.endpoint.is_some())
    }
}

fn str_param(p: &Params, key: &str) -> Option<String> {
    p.get(key).and_then(|v| v.as_str()).map(str::to_string)
}

fn bool_param(p: &Params, key: &str) -> Option<bool> {
    p.get(key).and_then(|v| v.as_bool())
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
    use crate::storage::multipart::{PartSink, StreamingMultipartWriter};
    use crate::storage::{MultipartWriter, Storage};
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
                if let (Some(ak), Some(sk)) = (&cfg.access_key_id, &cfg.secret_access_key) {
                    loader = loader.credentials_provider(aws_sdk_s3::config::Credentials::new(
                        ak.clone(),
                        sk.clone(),
                        None,
                        None,
                        "leafmask-config",
                    ));
                }
                let shared = loader.load().await;
                let conf = aws_sdk_s3::config::Builder::from(&shared)
                    .force_path_style(cfg.path_style())
                    .build();
                Client::from_conf(conf)
            });
            Ok(S3Storage { cfg, client, rt })
        }
    }

    struct S3PartSink {
        handle: tokio::runtime::Handle,
        client: Client,
        bucket: String,
        key: String,
        upload_id: String,
    }

    impl PartSink for S3PartSink {
        fn send_part(&mut self, part_number: u64, data: Vec<u8>) -> Result<String> {
            log::info!(
                "  sending part {part_number} ({:.1} MiB) to s3: {}/{}",
                data.len() as f64 / (1024.0 * 1024.0),
                self.bucket,
                self.key
            );
            let resp = self
                .handle
                .block_on(
                    self.client
                        .upload_part()
                        .bucket(&self.bucket)
                        .key(&self.key)
                        .upload_id(&self.upload_id)
                        .part_number(part_number as i32)
                        .body(ByteStream::from(data))
                        .send(),
                )
                .map_err(|e| Error::Storage(format!("upload_part: {e}")))?;
            resp.e_tag()
                .map(str::to_string)
                .ok_or_else(|| Error::Storage("upload_part response missing ETag".into()))
        }

        fn complete(self: Box<Self>, tokens: Vec<String>) -> Result<()> {
            use aws_sdk_s3::types::{CompletedMultipartUpload, CompletedPart};
            let parts: Vec<CompletedPart> = tokens
                .into_iter()
                .enumerate()
                .map(|(i, etag)| {
                    CompletedPart::builder()
                        .part_number(i as i32 + 1)
                        .e_tag(etag)
                        .build()
                })
                .collect();
            self.handle
                .block_on(
                    self.client
                        .complete_multipart_upload()
                        .bucket(&self.bucket)
                        .key(&self.key)
                        .upload_id(&self.upload_id)
                        .multipart_upload(
                            CompletedMultipartUpload::builder()
                                .set_parts(Some(parts))
                                .build(),
                        )
                        .send(),
                )
                .map_err(|e| Error::Storage(format!("complete_multipart_upload: {e}")))?;
            Ok(())
        }

        fn abort(&mut self) {
            let _ = self.handle.block_on(
                self.client
                    .abort_multipart_upload()
                    .bucket(&self.bucket)
                    .key(&self.key)
                    .upload_id(&self.upload_id)
                    .send(),
            );
        }
    }

    impl Storage for S3Storage {
        fn list_dumps(&self) -> Result<Vec<String>> {
            let prefix = join_key(&self.cfg.prefix, "");
            let mut ids: Vec<String> = Vec::new();
            let mut pages = self
                .client
                .list_objects_v2()
                .bucket(&self.cfg.bucket)
                .prefix(&prefix)
                .delimiter("/")
                .into_paginator()
                .send();
            self.rt.block_on(async {
                while let Some(page) = pages.next().await {
                    let page = page.map_err(|e| Error::Storage(e.to_string()))?;
                    ids.extend(
                        page.common_prefixes()
                            .iter()
                            .filter_map(|cp| cp.prefix())
                            .filter_map(|p| {
                                p.trim_end_matches('/')
                                    .rsplit('/')
                                    .next()
                                    .map(str::to_string)
                            }),
                    );
                }
                Ok::<(), Error>(())
            })?;
            ids.sort();
            Ok(ids)
        }

        fn get(&self, path: &str) -> Result<Vec<u8>> {
            let key = self.cfg.object_key(path);
            let resp = self
                .rt
                .block_on(
                    self.client
                        .get_object()
                        .bucket(&self.cfg.bucket)
                        .key(&key)
                        .send(),
                )
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
            match self.rt.block_on(
                self.client
                    .head_object()
                    .bucket(&self.cfg.bucket)
                    .key(&key)
                    .send(),
            ) {
                Ok(_) => Ok(true),
                Err(e) => {
                    let svc = e.into_service_error();
                    if svc.is_not_found() {
                        Ok(false)
                    } else {
                        Err(Error::Storage(svc.to_string()))
                    }
                }
            }
        }

        fn put_file(&self, path: &str, local: &std::path::Path) -> Result<()> {
            let key = self.cfg.object_key(path);
            // ByteStream::from_path streams the file from disk; the blob is
            // never buffered whole in memory.
            let body = self
                .rt
                .block_on(ByteStream::from_path(local))
                .map_err(|e| Error::Storage(e.to_string()))?;
            self.rt
                .block_on(
                    self.client
                        .put_object()
                        .bucket(&self.cfg.bucket)
                        .key(&key)
                        .body(body)
                        .send(),
                )
                .map_err(|e| Error::Storage(e.to_string()))?;
            Ok(())
        }

        fn get_reader(&self, path: &str) -> Result<Box<dyn std::io::Read + Send>> {
            use std::io::Write;

            let key = self.cfg.object_key(path);
            let resp = self
                .rt
                .block_on(
                    self.client
                        .get_object()
                        .bucket(&self.cfg.bucket)
                        .key(&key)
                        .send(),
                )
                .map_err(|_| Error::NotFound(format!("blob '{path}' not found")))?;
            // Spool the object to local disk chunk by chunk, then hand back a
            // file reader: bounded memory for arbitrarily large blobs.
            let (mut file, spool) = crate::storage::download_spool()?;
            let mut body = resp.body;
            while let Some(chunk) = self
                .rt
                .block_on(body.try_next())
                .map_err(|e| Error::Storage(e.to_string()))?
            {
                file.write_all(&chunk)
                    .map_err(|e| Error::Storage(e.to_string()))?;
            }
            drop(file);
            crate::storage::open_and_remove_spool(&spool)
        }

        fn delete(&self, prefix: &str) -> Result<()> {
            for rel in self.list(prefix)? {
                let key = self.cfg.object_key(&rel);
                self.rt
                    .block_on(
                        self.client
                            .delete_object()
                            .bucket(&self.cfg.bucket)
                            .key(&key)
                            .send(),
                    )
                    .map_err(|e| Error::Storage(e.to_string()))?;
            }
            Ok(())
        }

        fn list(&self, prefix: &str) -> Result<Vec<String>> {
            let full = self.cfg.object_key(prefix);
            let strip = join_key(&self.cfg.prefix, "");
            let mut out: Vec<String> = Vec::new();
            let mut pages = self
                .client
                .list_objects_v2()
                .bucket(&self.cfg.bucket)
                .prefix(&full)
                .into_paginator()
                .send();
            self.rt.block_on(async {
                while let Some(page) = pages.next().await {
                    let page = page.map_err(|e| Error::Storage(e.to_string()))?;
                    out.extend(
                        page.contents()
                            .iter()
                            .filter_map(|o| o.key())
                            .map(|k| k.strip_prefix(&strip).unwrap_or(k).to_string()),
                    );
                }
                Ok::<(), Error>(())
            })?;
            out.sort();
            Ok(out)
        }

        fn size(&self, prefix: &str) -> Result<u64> {
            let full = self.cfg.object_key(prefix);
            let mut total = 0u64;
            let mut pages = self
                .client
                .list_objects_v2()
                .bucket(&self.cfg.bucket)
                .prefix(&full)
                .into_paginator()
                .send();
            self.rt.block_on(async {
                while let Some(page) = pages.next().await {
                    let page = page.map_err(|e| Error::Storage(e.to_string()))?;
                    total += page
                        .contents()
                        .iter()
                        .map(|o| o.size().unwrap_or(0).max(0) as u64)
                        .sum::<u64>();
                }
                Ok::<(), Error>(())
            })?;
            Ok(total)
        }

        fn multipart_writer(&self, path: &str) -> Result<Option<Box<dyn MultipartWriter>>> {
            let key = self.cfg.object_key(path);
            let upload_id = self
                .rt
                .block_on(
                    self.client
                        .create_multipart_upload()
                        .bucket(&self.cfg.bucket)
                        .key(&key)
                        .send(),
                )
                .map_err(|e| Error::Storage(format!("create_multipart_upload: {e}")))?
                .upload_id()
                .ok_or_else(|| {
                    Error::Storage("create_multipart_upload response missing upload id".into())
                })?
                .to_string();

            let sink = S3PartSink {
                handle: self.rt.handle().clone(),
                client: self.client.clone(),
                bucket: self.cfg.bucket.clone(),
                key,
                upload_id,
            };
            Ok(Some(Box::new(StreamingMultipartWriter::spawn(Box::new(
                sink,
            )))))
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
        assert_eq!(
            cfg.object_key("dump-1/metadata.json"),
            "leafmask/dump-1/metadata.json"
        );

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

    // Custom endpoints default to path-style addressing (MinIO & friends);
    // AWS-proper (no endpoint) keeps virtual-host style.
    #[test]
    fn path_style_defaults_follow_endpoint() {
        let with_ep =
            S3Config::from_params(&params("bucket: b\nendpoint: http://minio:9000\n")).unwrap();
        assert!(with_ep.path_style());
        let plain = S3Config::from_params(&params("bucket: b\n")).unwrap();
        assert!(!plain.path_style());
    }

    // force_path_style overrides the default in both directions.
    #[test]
    fn force_path_style_overrides_default() {
        let off = S3Config::from_params(&params(
            "bucket: b\nendpoint: http://r2.example.com\nforce_path_style: false\n",
        ))
        .unwrap();
        assert!(!off.path_style());
        let on = S3Config::from_params(&params("bucket: b\nforce_path_style: true\n")).unwrap();
        assert!(on.path_style());
    }
}
