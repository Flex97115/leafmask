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
    assert!(m.databases().contains(&db));
    assert_eq!(m.collections(&db), vec!["users".to_string()]);

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
