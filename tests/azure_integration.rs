//! Integration test for the Azure storage backend's streaming multipart
//! (block blob) writer, run against a real (containerized) Azurite
//! instance — Microsoft's official Azure Storage emulator.
//!
//! ```sh
//! cargo test --features "azure,integration-tests" --test azure_integration
//! ```
//!
//! `testcontainers-modules` starts and tears down the Azurite container
//! from inside the test itself — no manual `docker run` step needed, only
//! a working local Docker daemon.
#![cfg(all(feature = "azure", feature = "integration-tests"))]

use std::collections::BTreeMap;

use bson::{doc, Document};
use leafmask::dump::{list_metadata, read_collection_full, Dump, DumpOptions};
use leafmask::hash::HashEngine;
use leafmask::mongo::{CollectionData, InMemoryMongo};
use leafmask::storage::azure::{AzureConfig, AzureStorage};
use leafmask::storage::Storage;
use leafmask::transform::Registry;
use sha2::{Digest, Sha256};
use testcontainers_modules::azurite::{Azurite, BLOB_PORT};
use testcontainers_modules::testcontainers::runners::SyncRunner;

// A per-document hash-derived hex string. Sequential `_id`s and predictable
// `u{i}@x.com` emails alone compress via gzip so well (>99%) that 200k of
// them fit in a single 8 MiB block — defeating the point of this test,
// which is to prove *real* multi-block upload/reassembly against Azurite.
// The high-entropy padding keeps the gzip'd blob comfortably above several
// block boundaries.
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

// Real Azurite, real block-blob multipart-style upload: dumps a collection
// large enough to span several 8 MiB blocks, then reads it back and checks
// every document round-trips. This exercises
// StreamingMultipartWriter/AzurePartSink against a real Azure Blob-API-
// compatible service, not a fake.
#[test]
fn dump_round_trips_through_real_azurite_block_upload() {
    // `log::info!` calls (like AzurePartSink's "sending block N" lines) are
    // no-ops without a registered logger — `src/main.rs` initializes one
    // for the CLI binary, but this test binary needs its own so
    // `--nocapture` actually shows the multipart evidence.
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .is_test(true)
        .try_init();

    let azurite = Azurite::default().start().expect("start Azurite container");
    let host = azurite.get_host().expect("azurite host");
    let port = azurite
        .get_host_port_ipv4(BLOB_PORT)
        .expect("azurite blob port");

    // Container creation isn't part of the Storage trait (production
    // containers are pre-provisioned) — create it directly, the same way
    // AzureStorage::open builds its client for the emulator branch.
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        use azure_storage::prelude::*;
        use azure_storage::CloudLocation;
        use azure_storage_blobs::prelude::*;

        ClientBuilder::with_location(
            CloudLocation::Emulator {
                address: host.to_string(),
                port,
            },
            StorageCredentials::emulator(),
        )
        .container_client("leafmask-test")
        .create()
        .into_future()
        .await
        .expect("create test container");
    });

    let cfg = AzureConfig {
        account: None,
        container: "leafmask-test".into(),
        access_key: None,
        prefix: "".into(),
        emulator_host: Some(host.to_string()),
        emulator_port: Some(port),
    };
    let storage = AzureStorage::open(cfg).expect("open AzureStorage against Azurite");

    // 200k small documents comfortably spans multiple 8 MiB blocks.
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
    let meta = dump.run(chrono::Utc::now()).expect("dump against Azurite");

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
