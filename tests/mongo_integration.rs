//! Integration tests for the real MongoDB adapter (`MongoDriver`).
//!
//! These run only with the `mongo` feature and a reachable MongoDB, e.g.:
//!
//! ```sh
//! docker run -d --name leafmask-mongo -p 27017:27017 mongo:7
//! cargo test --features mongo --test mongo_integration
//! ```
//!
//! The URI is taken from `LEAFMASK_MONGO_URI` (default `mongodb://localhost:27017`).
//! Each test uses a uniquely-named database and drops it afterwards.
#![cfg(feature = "mongo")]

use std::collections::BTreeMap;

use bson::{doc, Bson, Document};
use leafmask::dump::{list_metadata, read_collection_full, Dump, DumpOptions};
use leafmask::hash::HashEngine;
use leafmask::mongo::{MongoDriver, MongoSink, MongoSource};
use leafmask::restore::{ErrorExclusions, Restore, RestoreOptions};
use leafmask::storage::DirectoryStorage;
use leafmask::transform::{apply::TransformationPlan, Registry};
use leafmask::validate::IndexSpec;

fn uri() -> String {
    std::env::var("LEAFMASK_MONGO_URI").unwrap_or_else(|_| "mongodb://localhost:27017".into())
}

/// A unique database name per test, so parallel tests do not collide.
fn db_name(tag: &str) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("leafmask_it_{tag}_{}_{}", std::process::id(), nanos)
}

fn connect() -> MongoDriver {
    MongoDriver::connect(&uri()).expect("connect to MongoDB (is the container running?)")
}

fn user(id: i64, email: &str) -> Document {
    doc! { "_id": id, "email": email }
}

// The adapter implements both traits: write via the sink, read it back via the
// source, including the created index.
#[test]
fn sink_and_source_round_trip_through_real_mongo() {
    let m = connect();
    let db = db_name("rt");
    let idx = IndexSpec {
        name: "email_idx".into(),
        keys: vec![("email".into(), 1)],
        unique: true,
    };

    m.ensure_collection(&db, "users", &None, &BTreeMap::new())
        .unwrap();
    m.insert(&db, "users", &user(1, "a@x.com")).unwrap();
    m.insert(&db, "users", &user(2, "b@x.com")).unwrap();
    m.create_index(&db, "users", &idx).unwrap();

    // enumeration sees the db and collection.
    assert!(m.databases().unwrap().contains(&db));
    assert_eq!(m.collections(&db).unwrap(), vec!["users".to_string()]);

    // read back documents + the index we created.
    let data = m.read_collection(&db, "users").unwrap();
    assert_eq!(data.documents.len(), 2);
    assert!(data
        .indexes
        .iter()
        .any(|i| i.name == "email_idx" && i.unique));

    m.drop_database(&db).unwrap();
}

// A duplicate `_id` insert surfaces as an InsertError with code 11000 and the
// offending index name, which is what error-exclusions match on.
#[test]
fn duplicate_key_insert_reports_error() {
    let m = connect();
    let db = db_name("dup");
    m.insert(&db, "users", &user(1, "a@x.com")).unwrap();
    let err = m.insert(&db, "users", &user(1, "again@x.com")).unwrap_err();
    assert_eq!(err.code, Some(11000));
    assert_eq!(err.index_name.as_deref(), Some("_id_"));
    m.drop_database(&db).unwrap();
}

// End-to-end: dump a live database to directory storage, drop it, restore it,
// and confirm the documents come back — the whole product against real MongoDB.
#[test]
fn dump_then_restore_round_trip() {
    let m = connect();
    let db = db_name("e2e");
    m.ensure_collection(&db, "users", &None, &BTreeMap::new())
        .unwrap();
    for i in 1..=3 {
        m.insert(&db, "users", &user(i, &format!("u{i}@x.com")))
            .unwrap();
    }
    m.create_index(
        &db,
        "users",
        &IndexSpec {
            name: "email_idx".into(),
            keys: vec![("email".into(), 1)],
            unique: true,
        },
    )
    .unwrap();

    let dir = tempfile::tempdir().unwrap();
    let storage = DirectoryStorage::new(dir.path()).unwrap();
    let registry = Registry::with_builtins();
    let engine = HashEngine::new("salt");

    // Dump only our test db.
    let meta = Dump {
        storage: &storage,
        source: &m,
        registry: &registry,
        engine: &engine,
        plan: None,
        filters: BTreeMap::new(),
        options: DumpOptions {
            tmp_dir: Some(dir.path().display().to_string()),
            include_databases: vec![db.clone()],
            ..Default::default()
        },
    }
    .run(chrono::Utc::now())
    .unwrap();
    assert_eq!(meta.databases.len(), 1);

    // Wipe the live db, then restore it from storage.
    m.drop_database(&db).unwrap();
    assert!(
        m.read_collection(&db, "users")
            .map(|c| c.documents.len())
            .unwrap_or(0)
            == 0
    );

    let report = Restore {
        storage: &storage,
        sink: &m,
        exclusions: ErrorExclusions::default(),
        scripts: Default::default(),
        options: RestoreOptions {
            batch_size: 10,
            ..Default::default()
        },
    }
    .run(
        &meta.id,
        &leafmask::restore::ProcessScriptRunner::new(uri()),
    )
    .unwrap();
    // pre/post script stages are empty here, so ProcessScriptRunner is never invoked.
    assert_eq!(report.inserted, 3);

    let restored = m.read_collection(&db, "users").unwrap();
    assert_eq!(restored.documents.len(), 3);
    assert!(restored.indexes.iter().any(|i| i.name == "email_idx"));

    m.drop_database(&db).unwrap();
}

// Transformations run inline against a real read: the dumped email is masked.
#[test]
fn transformation_applied_on_real_dump() {
    let m = connect();
    let db = db_name("xf");
    m.insert(&db, "users", &user(1, "secret@x.com")).unwrap();

    let dir = tempfile::tempdir().unwrap();
    let storage = DirectoryStorage::new(dir.path()).unwrap();
    let registry = Registry::with_builtins();
    let engine = HashEngine::new("salt");
    let configs: Vec<leafmask::transform::apply::TransformationConfig> = serde_yaml::from_str(
        "- collection: users\n  transformers:\n    - field: email\n      name: masking\n",
    )
    .unwrap();
    let plan = TransformationPlan::compile(&configs, &registry, &engine).unwrap();

    let meta = Dump {
        storage: &storage,
        source: &m,
        registry: &registry,
        engine: &engine,
        plan: Some(&plan),
        filters: BTreeMap::new(),
        options: DumpOptions {
            tmp_dir: Some(dir.path().display().to_string()),
            include_databases: vec![db.clone()],
            ..Default::default()
        },
    }
    .run(chrono::Utc::now())
    .unwrap();

    let full = read_collection_full(&storage, &meta.id, &db, "users").unwrap();
    let email = full.documents[0].get_str("email").unwrap();
    assert!(
        email.chars().all(|c| c == '*') && !email.is_empty(),
        "email not masked: {email}"
    );
    let _ = Bson::Null;

    m.drop_database(&db).unwrap();
}

// A `dump.include_databases` config entry, with no `--include-db` flag on
// the command line, is enough to restrict which database gets dumped — the
// scenario that motivated moving these filters into config (avoids spelling
// out every database/collection on the CLI).
#[test]
fn dump_filters_from_config_include_databases_without_cli_flag() {
    let m = connect();
    let wanted = db_name("cfgfilt_keep");
    let skipped = db_name("cfgfilt_skip");
    m.ensure_collection(&wanted, "users", &None, &BTreeMap::new())
        .unwrap();
    m.insert(&wanted, "users", &user(1, "a@x.com")).unwrap();
    m.ensure_collection(&skipped, "users", &None, &BTreeMap::new())
        .unwrap();
    m.insert(&skipped, "users", &user(2, "b@x.com")).unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let storage_path = tmp.path().join("store");
    let work_dir = tmp.path().join("work");
    std::fs::create_dir_all(&work_dir).unwrap();
    let config_path = tmp.path().join("leafmask.yaml");
    std::fs::write(
        &config_path,
        format!(
            "common:\n  tmp_dir: {}\nstorage:\n  type: directory\n  path: {}\ndump:\n  include_databases: [{}]\n",
            work_dir.display(),
            storage_path.display(),
            wanted,
        ),
    )
    .unwrap();

    let cli = leafmask::cli::Cli {
        config: Some(config_path),
        uri: Some(uri()),
        command: leafmask::cli::Command::Dump(leafmask::cli::DumpArgs {
            gzip: false,
            include_db: vec![],
            exclude_db: vec![],
            include_collection: vec![],
            exclude_collection: vec![],
            jobs: 1,
            no_indexes: false,
        }),
    };
    leafmask::cli::run(cli).unwrap();

    let storage = leafmask::storage::DirectoryStorage::new(&storage_path).unwrap();
    let dumps = list_metadata(&storage).unwrap();
    assert_eq!(dumps.len(), 1);
    let db_names: Vec<&str> = dumps[0].databases.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(db_names, vec![wanted.as_str()]);

    m.drop_database(&wanted).unwrap();
    m.drop_database(&skipped).unwrap();
}

// A larger round-trip through the real driver: thousands of documents are
// dumped (gzip, streamed) and restored in insert_many batches — the 10M+
// collection path in miniature. The restored count and a spot-checked document
// must match exactly.
#[test]
fn large_dump_restore_round_trip_in_batches() {
    let m = connect();
    let db = db_name("big");
    const N: i64 = 5_000;

    // Seed via bulk inserts (also exercises the driver's insert_many).
    let docs: Vec<Document> = (0..N)
        .map(|i| doc! { "_id": i, "email": format!("u{i}@x.com"), "n": i })
        .collect();
    for chunk in docs.chunks(1_000) {
        let out = m.insert_many(&db, "users", chunk, false).unwrap();
        assert_eq!(out.inserted, chunk.len() as u64);
        assert!(out.failures.is_empty());
    }

    let dir = tempfile::tempdir().unwrap();
    let storage = DirectoryStorage::new(dir.path()).unwrap();
    let registry = Registry::with_builtins();
    let engine = HashEngine::new("salt");
    let meta = Dump {
        storage: &storage,
        source: &m,
        registry: &registry,
        engine: &engine,
        plan: None,
        filters: BTreeMap::new(),
        options: DumpOptions {
            tmp_dir: Some(dir.path().display().to_string()),
            include_databases: vec![db.clone()],
            gzip: true,
            ..Default::default()
        },
    }
    .run(chrono::Utc::now())
    .unwrap();
    assert_eq!(meta.databases[0].collections[0].document_count, N as u64);

    m.drop_database(&db).unwrap();

    let report = Restore {
        storage: &storage,
        sink: &m,
        exclusions: ErrorExclusions::default(),
        scripts: Default::default(),
        options: RestoreOptions {
            batch_size: 500,
            ..Default::default()
        },
    }
    .run(
        &meta.id,
        &leafmask::restore::ProcessScriptRunner::new(uri()),
    )
    .unwrap();
    assert_eq!(report.inserted, N as u64);
    assert!(report.failed.is_empty());

    let restored = m.read_collection(&db, "users").unwrap();
    assert_eq!(restored.documents.len(), N as usize);
    let one = restored
        .documents
        .iter()
        .find(|d| d.get_i64("_id") == Ok(4_242))
        .expect("doc 4242 restored");
    assert_eq!(one.get_str("email").unwrap(), "u4242@x.com");

    m.drop_database(&db).unwrap();
}

// The driver's insert_many surfaces per-document failures (index + code) so
// error exclusions can tolerate them; unordered attempts every document.
#[test]
fn insert_many_reports_indexed_failures() {
    let m = connect();
    let db = db_name("bulk");
    m.insert(&db, "users", &user(1, "a@x.com")).unwrap();

    let batch = vec![user(0, "z@x.com"), user(1, "dup@x.com"), user(2, "b@x.com")];

    // Unordered: both non-duplicates go in; the duplicate is reported with its
    // batch index and code 11000.
    let out = m.insert_many(&db, "users", &batch, false).unwrap();
    assert_eq!(out.inserted, 2);
    assert_eq!(out.failures.len(), 1);
    let (idx, err) = &out.failures[0];
    assert_eq!(*idx, 1);
    assert_eq!(err.code, Some(11000));
    assert_eq!(err.index_name.as_deref(), Some("_id_"));

    m.drop_database(&db).unwrap();

    // Ordered: the batch stops at the duplicate; the document after it is not
    // attempted.
    let m2 = connect();
    let db2 = db_name("bulk_ord");
    m2.insert(&db2, "users", &user(1, "a@x.com")).unwrap();
    let out = m2.insert_many(&db2, "users", &batch, true).unwrap();
    assert_eq!(out.inserted, 1);
    assert_eq!(out.failures.len(), 1);
    assert_eq!(out.failures[0].0, 1);
    assert_eq!(
        m2.read_collection(&db2, "users").unwrap().documents.len(),
        2 // the pre-existing doc + batch[0]
    );
    m2.drop_database(&db2).unwrap();
}

// validate --data must fetch only the sampled documents, with the limit pushed
// down to the server.
#[test]
fn read_sample_limits_fetched_documents() {
    let m = connect();
    let db = db_name("sample");
    for i in 0..50i64 {
        m.insert(&db, "events", &doc! { "_id": i }).unwrap();
    }
    let sample = m.read_sample(&db, "events", 7).unwrap();
    assert_eq!(sample.len(), 7);
    m.drop_database(&db).unwrap();
}

// subset_conds must actually narrow what's fetched from MongoDB, not just what
// ends up in the dump file — otherwise a filter on a huge collection still
// pulls every document into memory before discarding most of them. This drives
// the real driver's find() with a translated filter and checks both the raw
// read and a full Dump only see the matching documents.
#[test]
fn subset_filter_pushes_down_to_mongo_query() {
    use leafmask::transform::condition::Condition;

    let m = connect();
    let db = db_name("filter");
    for i in 1..=5i64 {
        m.insert(&db, "events", &doc! { "_id": i, "value": i })
            .unwrap();
    }

    let filter = Condition::parse("value >= 3").unwrap().to_filter();

    // Low-level: the driver itself only returns matching documents.
    let filtered = m
        .read_collection_filtered(&db, "events", Some(&filter))
        .unwrap();
    let mut values: Vec<i64> = filtered
        .documents
        .iter()
        .map(|d| d.get_i64("value").unwrap())
        .collect();
    values.sort();
    assert_eq!(values, vec![3, 4, 5]);

    // End-to-end: a Dump with this filter set for the collection only dumps
    // the matching documents, exactly like `subset_conds` should.
    let dir = tempfile::tempdir().unwrap();
    let storage = DirectoryStorage::new(dir.path()).unwrap();
    let registry = Registry::with_builtins();
    let engine = HashEngine::new("salt");
    let mut filters = BTreeMap::new();
    filters.insert("events".to_string(), filter);

    let meta = Dump {
        storage: &storage,
        source: &m,
        registry: &registry,
        engine: &engine,
        plan: None,
        filters,
        options: DumpOptions {
            tmp_dir: Some(dir.path().display().to_string()),
            include_databases: vec![db.clone()],
            ..Default::default()
        },
    }
    .run(chrono::Utc::now())
    .unwrap();

    let full = read_collection_full(&storage, &meta.id, &db, "events").unwrap();
    assert_eq!(full.documents.len(), 3);

    m.drop_database(&db).unwrap();
}

// End-to-end bench on a small volume: seeds, dumps, restores, verifies the
// count, cleans up after itself.
#[test]
fn bench_round_trips_and_cleans_up() {
    use leafmask::bench::{run_bench, BenchOptions};

    let m = connect();
    let runs = run_bench(
        &m,
        &BenchOptions {
            sizes: vec![1_000],
            keep: false,
        },
    )
    .unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].docs, 1_000);
    assert!(!runs[0].estimated);
    assert!(runs[0].dump_bytes > 0);
    assert!(runs[0].dump_secs > 0.0 && runs[0].restore_secs > 0.0);
    // The bench database must be gone afterwards.
    assert!(!m.databases().unwrap().iter().any(|d| d == "leafmask_bench"));
}
