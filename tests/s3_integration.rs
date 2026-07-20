//! Integration test for the S3 storage backend's streaming multipart
//! writer, run against a real (containerized) MinIO instance.
//!
//! ```sh
//! cargo test --features "s3,integration-tests" --test s3_integration
//! ```
//!
//! `testcontainers-modules` starts and tears down the MinIO container from
//! inside the test itself — no manual `docker run` step needed, only a
//! working local Docker daemon.
#![cfg(all(feature = "s3", feature = "integration-tests"))]

use std::collections::BTreeMap;

use bson::{doc, Document};
use leafmask::dump::{list_metadata, read_collection_full, Dump, DumpOptions};
use leafmask::hash::HashEngine;
use leafmask::mongo::{CollectionData, InMemoryMongo};
use leafmask::storage::s3::{S3Config, S3Storage};
use leafmask::storage::Storage;
use leafmask::transform::Registry;
use sha2::{Digest, Sha256};
use testcontainers_modules::{minio::MinIO, testcontainers::runners::SyncRunner};

// A per-document hash-derived hex string. Sequential `_id`s and predictable
// `u{i}@x.com` emails alone compress via gzip so well (>99%) that 200k of
// them fit in a single 8 MiB multipart part — defeating the point of this
// test, which is to prove *real* multi-part upload/reassembly against
// MinIO. The high-entropy padding keeps the gzip'd blob comfortably above
// several part boundaries.
fn padding(i: i64) -> String {
    let mut hasher = Sha256::new();
    hasher.update(i.to_le_bytes());
    hex::encode(hasher.finalize())
}

fn users(n: i64) -> Vec<Document> {
    (0..n)
        .map(|i| doc! { "_id": i, "email": format!("u{i}@x.com"), "padding": padding(i) })
        .collect()
}

// Real MinIO, real multipart upload: dumps a collection large enough to
// span several 8 MiB parts, then reads it back and checks every document
// round-trips. This is what actually exercises
// StreamingMultipartWriter/S3PartSink against a real S3-compatible API,
// not a fake.
#[test]
fn dump_round_trips_through_real_minio_multipart_upload() {
    // `log::info!` calls (like S3PartSink's "sending part N" lines) are
    // no-ops without a registered logger — `src/main.rs` initializes one
    // for the CLI binary, but this test binary needs its own so
    // `--nocapture` actually shows the multipart evidence.
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .is_test(true)
        .try_init();

    let minio = MinIO::default().start().expect("start MinIO container");
    let host = minio.get_host().expect("minio host");
    let port = minio.get_host_port_ipv4(9000).expect("minio api port");
    let endpoint = format!("http://{host}:{port}");

    // Bucket creation isn't part of the Storage trait (production buckets
    // are pre-provisioned) — create it directly, the same way
    // S3Storage::open builds its client.
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let loader = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(aws_config::Region::new("us-east-1"))
            .endpoint_url(&endpoint)
            .credentials_provider(aws_sdk_s3::config::Credentials::new(
                "minioadmin",
                "minioadmin",
                None,
                None,
                "test",
            ));
        let shared = loader.load().await;
        let conf = aws_sdk_s3::config::Builder::from(&shared)
            .force_path_style(true)
            .build();
        aws_sdk_s3::Client::from_conf(conf)
            .create_bucket()
            .bucket("leafmask-test")
            .send()
            .await
            .expect("create test bucket");
    });

    let cfg = S3Config {
        bucket: "leafmask-test".into(),
        region: Some("us-east-1".into()),
        endpoint: Some(endpoint),
        prefix: "".into(),
        access_key_id: Some("minioadmin".into()),
        secret_access_key: Some("minioadmin".into()),
        force_path_style: Some(true),
    };
    let storage = S3Storage::open(cfg).expect("open S3Storage against MinIO");

    // 200k small documents comfortably spans multiple 8 MiB parts.
    let mongo = InMemoryMongo::new();
    mongo.seed(CollectionData {
        database: "app".into(),
        collection: "users".into(),
        documents: users(200_000),
        ..Default::default()
    });

    let registry = Registry::with_builtins();
    let engine = HashEngine::new("s");
    let dir = tempfile::tempdir().unwrap();
    let options = DumpOptions {
        tmp_dir: Some(dir.path().display().to_string()),
        gzip: true,
        ..Default::default()
    };
    let dump = Dump {
        storage: &storage,
        source: &mongo,
        registry: &registry,
        engine: &engine,
        plan: None,
        filters: BTreeMap::new(),
        options,
    };
    let meta = dump.run(chrono::Utc::now()).expect("dump against MinIO");

    // The data blob must exceed one part's worth of bytes (8 MiB —
    // StreamingMultipartWriter's INITIAL_PART_SIZE in
    // src/storage/multipart.rs) so this test actually proves multi-part
    // reassembly, not just that the multipart code path was reached once.
    let data_size = storage
        .size(&format!("{}/data/app/users.bson.gz", meta.id))
        .unwrap();
    assert!(
        data_size > 8 * 1024 * 1024,
        "expected the data blob to span more than one 8 MiB part, got {data_size} bytes — the test's document padding may have become too compressible"
    );

    let full = read_collection_full(&storage, &meta.id, "app", "users").unwrap();
    assert_eq!(full.documents.len(), 200_000);
    assert_eq!(full.documents[0].get_i64("_id").unwrap(), 0);
    assert_eq!(full.documents[199_999].get_i64("_id").unwrap(), 199_999);

    let dumps = list_metadata(&storage).unwrap();
    assert_eq!(dumps.len(), 1);
}
