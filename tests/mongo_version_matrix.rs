//! Cross-version integration tests: the contract Leafmask relies on from the
//! MongoDB server itself.
//!
//! CI runs this file (and the rest of the `mongo`-gated suite) once per entry
//! in [`support::SUPPORTED_SERVER_VERSIONS`], so a server release that changes
//! error codes, catalog shapes, or BSON round-trip fidelity fails here rather
//! than in someone's staging restore. Adding a future MongoDB release means
//! adding it to that list and to the CI matrix — at which point every test
//! below must pass against it before the release is claimed as supported.
//!
//! ```sh
//! docker run -d --rm -p 27017:27017 mongo:8
//! cargo test --features mongo --test mongo_version_matrix
//!
//! # or the whole matrix, one server version at a time:
//! ./scripts/test-mongo-matrix.sh
//! ```
#![cfg(feature = "mongo")]

mod support;

use std::collections::BTreeMap;

use bson::{doc, Bson, Document};
use leafmask::mongo::{CollectionData, MongoSink, MongoSource};
use leafmask::validate::IndexSpec;
use support::{server_version, TestDb, SUPPORTED_SERVER_VERSIONS};

/// The server under test must be one Leafmask claims to support. This is the
/// tripwire: pointing CI at a MongoDB release nobody has vetted fails loudly
/// instead of quietly passing and implying support that was never verified.
#[test]
fn server_under_test_is_a_supported_release() {
    let (major, minor, patch) = server_version();
    assert!(
        SUPPORTED_SERVER_VERSIONS.contains(&(major, minor)),
        "connected to MongoDB {major}.{minor}.{patch}, which is not in the supported set \
         {SUPPORTED_SERVER_VERSIONS:?}. If this release is meant to be supported, add it to \
         SUPPORTED_SERVER_VERSIONS in tests/support/mod.rs and to the CI matrix, then make \
         the suite pass against it."
    );
}

/// Every BSON type Leafmask writes must survive a MongoDB round trip
/// unchanged. Encoding fidelity is the assumption underneath every dump, and
/// it is exactly the kind of thing a server or driver upgrade can shift
/// without any code change here.
#[test]
fn every_bson_type_round_trips_through_this_server_version() {
    let db = TestDb::new("bson_fidelity");
    let driver = db.driver();

    let original = doc! {
        "_id": 1i32,
        "double": 3.5f64,
        "double_negative": -0.125f64,
        "string": "héllo wörld \u{1F343}",
        "empty_string": "",
        "document": { "nested": { "deeper": 1i32 } },
        "empty_document": {},
        "array": [1i32, "two", 3.0f64, Bson::Null],
        "empty_array": Bson::Array(vec![]),
        "binary": Bson::Binary(bson::Binary {
            subtype: bson::spec::BinarySubtype::Generic,
            bytes: vec![0, 1, 2, 253, 254, 255],
        }),
        "object_id": bson::oid::ObjectId::from_bytes([9u8; 12]),
        "boolean_true": true,
        "boolean_false": false,
        "datetime": bson::DateTime::from_millis(1_600_000_000_123),
        "datetime_epoch": bson::DateTime::from_millis(0),
        "datetime_pre_epoch": bson::DateTime::from_millis(-1_000_000_000),
        "null": Bson::Null,
        "regex": Bson::RegularExpression(bson::Regex {
            pattern: "^a.*z$".into(),
            options: "i".into(),
        }),
        "int32": i32::MIN,
        "int32_max": i32::MAX,
        "int64": i64::MIN,
        "int64_max": i64::MAX,
        "timestamp": Bson::Timestamp(bson::Timestamp { time: 42, increment: 7 }),
        "decimal128": Bson::Decimal128("123.456".parse().expect("decimal128")),
    };

    driver
        .ensure_collection(&db.name, "types", &None, &BTreeMap::new())
        .expect("create collection");
    driver
        .insert(&db.name, "types", &original)
        .expect("insert every BSON type");

    let read_back = driver
        .read_collection(&db.name, "types")
        .expect("read collection back");
    assert_eq!(read_back.documents.len(), 1);
    let got = &read_back.documents[0];

    // Compare field by field so a failure names the type that drifted.
    for (key, expected) in original.iter() {
        let actual = got.get(key).unwrap_or_else(|| {
            panic!(
                "field '{key}' missing after round trip on MongoDB {:?}",
                server_version()
            )
        });
        assert_eq!(
            actual,
            expected,
            "field '{key}' changed across a MongoDB {:?} round trip",
            server_version()
        );
    }
    assert_eq!(got.len(), original.len(), "document gained or lost fields");
}

/// The duplicate-key error code restore keys its behaviour off. `restore`
/// tolerates configured error codes; if the server ever renumbered this one,
/// `--on-conflict`-style exclusions would silently stop matching and a restore
/// would abort where it used to continue.
#[test]
fn duplicate_key_error_code_is_stable_across_versions() {
    let db = TestDb::new("dup_key_code");
    let driver = db.driver();

    driver
        .ensure_collection(&db.name, "users", &None, &BTreeMap::new())
        .expect("create collection");
    let d = doc! { "_id": 1i32, "email": "a@example.com" };
    driver.insert(&db.name, "users", &d).expect("first insert");

    let err = driver
        .insert(&db.name, "users", &d)
        .expect_err("second insert must be a duplicate key error");
    assert_eq!(
        err.code,
        Some(11000),
        "duplicate key code changed on MongoDB {:?}",
        server_version()
    );
}

/// A unique index violation must name the offending index, on every server
/// version. `restore --exclude-index`-style error exclusions match on that
/// name; a server that stopped reporting it would break them silently.
#[test]
fn unique_index_violation_reports_the_index_name() {
    let db = TestDb::new("index_name");
    let driver = db.driver();

    driver
        .ensure_collection(&db.name, "users", &None, &BTreeMap::new())
        .expect("create collection");
    driver
        .create_index(
            &db.name,
            "users",
            &IndexSpec {
                name: "email_unique".into(),
                keys: vec![("email".into(), 1)],
                unique: true,
            },
        )
        .expect("create unique index");

    driver
        .insert(
            &db.name,
            "users",
            &doc! { "_id": 1i32, "email": "a@example.com" },
        )
        .expect("first insert");
    let err = driver
        .insert(
            &db.name,
            "users",
            &doc! { "_id": 2i32, "email": "a@example.com" },
        )
        .expect_err("duplicate email must fail");

    assert_eq!(err.code, Some(11000));
    assert_eq!(
        err.index_name.as_deref(),
        Some("email_unique"),
        "index name missing from the write error on MongoDB {:?}; error was {err:?}",
        server_version()
    );
}

/// Unordered batch inserts must attempt every document and report each
/// failure; ordered ones must stop at the first. Restore's progress accounting
/// and its "tolerated errors" reporting both depend on these counts being
/// exact, and both are server-side semantics.
#[test]
fn batch_insert_ordering_semantics_are_stable() {
    let db = TestDb::new("batch_semantics");
    let driver = db.driver();

    driver
        .ensure_collection(&db.name, "users", &None, &BTreeMap::new())
        .expect("create collection");
    driver
        .insert(&db.name, "users", &doc! { "_id": 2i32 })
        .expect("seed the conflicting id");

    // Unordered: ids 1 and 3 go in, id 2 conflicts.
    let batch = vec![
        doc! { "_id": 1i32 },
        doc! { "_id": 2i32 },
        doc! { "_id": 3i32 },
    ];
    let unordered = driver
        .insert_many(&db.name, "users", &batch, false)
        .expect("unordered batch");
    assert_eq!(
        (unordered.inserted, unordered.failures.len()),
        (2, 1),
        "unordered batch semantics changed on MongoDB {:?}: {unordered:?}",
        server_version()
    );
    assert_eq!(
        unordered.failures[0].0, 1,
        "failure index must be positional"
    );

    // Ordered: the batch stops at the conflict, so only what preceded it lands.
    let db2 = TestDb::new("batch_ordered");
    let driver2 = db2.driver();
    driver2
        .ensure_collection(&db2.name, "users", &None, &BTreeMap::new())
        .expect("create collection");
    driver2
        .insert(&db2.name, "users", &doc! { "_id": 2i32 })
        .expect("seed the conflicting id");
    let ordered = driver2
        .insert_many(&db2.name, "users", &batch, true)
        .expect("ordered batch");
    assert_eq!(
        ordered.inserted,
        1,
        "ordered batch must stop at the first failure on MongoDB {:?}: {ordered:?}",
        server_version()
    );
}

/// Collection validators and index options must survive being read from one
/// collection and applied to another — that is precisely what restore does,
/// and validator handling has shifted between server generations before.
#[test]
fn validators_and_indexes_survive_a_structure_round_trip() {
    let db = TestDb::new("structure");
    let driver = db.driver();

    let validator = Some(Bson::Document(doc! {
        "$jsonSchema": {
            "bsonType": "object",
            "required": ["email"],
            "properties": { "email": { "bsonType": "string" } },
        }
    }));
    driver
        .ensure_collection(&db.name, "source", &validator, &BTreeMap::new())
        .expect("create validated collection");
    driver
        .create_index(
            &db.name,
            "source",
            &IndexSpec {
                name: "email_unique".into(),
                keys: vec![("email".into(), 1)],
                unique: true,
            },
        )
        .expect("create index");
    driver
        .insert(
            &db.name,
            "source",
            &doc! { "_id": 1i32, "email": "a@example.com" },
        )
        .expect("insert a valid document");

    let structure = driver
        .read_structure(&db.name, "source")
        .expect("read structure");
    assert!(
        structure.validator.is_some(),
        "validator lost on read on MongoDB {:?}",
        server_version()
    );
    let email_index = structure
        .indexes
        .iter()
        .find(|i| i.name == "email_unique")
        .expect("email_unique index present in the read-back structure");
    assert!(email_index.unique, "unique flag lost on read");
    assert_eq!(email_index.keys, vec![("email".to_string(), 1)]);

    // Re-apply it to a fresh collection, as restore would.
    driver
        .ensure_collection(&db.name, "target", &structure.validator, &structure.options)
        .expect("recreate collection from the read-back structure");
    for index in &structure.indexes {
        if index.name == "_id_" {
            continue; // implicit, always created by the server.
        }
        driver
            .create_index(&db.name, "target", index)
            .expect("recreate index");
    }

    // The re-applied validator must actually be enforced, or the "restore
    // preserves structure" promise is cosmetic.
    let rejected = driver.insert(&db.name, "target", &doc! { "_id": 1i32, "email": 42i32 });
    assert!(
        rejected.is_err(),
        "re-applied validator not enforced on MongoDB {:?}",
        server_version()
    );
}

/// A full dump/restore cycle against this server version, end to end through
/// the real driver. Each matrix leg runs it, so the whole pipeline — not just
/// isolated driver calls — is what gets certified against a server release.
#[test]
fn dump_restore_round_trip_on_this_server_version() {
    use leafmask::dump::{Dump, DumpOptions};
    use leafmask::hash::HashEngine;
    use leafmask::restore::{ErrorExclusions, Restore, RestoreOptions};
    use leafmask::storage::DirectoryStorage;
    use leafmask::transform::Registry;

    let db = TestDb::new("e2e");
    let driver = db.driver();

    let documents: Vec<Document> = (0..250)
        .map(|i| doc! { "_id": i, "email": format!("user{i}@example.com"), "n": i as i64 })
        .collect();
    driver
        .ensure_collection(&db.name, "users", &None, &BTreeMap::new())
        .expect("create source collection");
    driver
        .insert_many(&db.name, "users", &documents, true)
        .expect("seed source data");

    let tmp = tempfile::tempdir().expect("tempdir");
    let storage = DirectoryStorage::new(tmp.path()).expect("directory storage");
    let engine = HashEngine::new("matrix-salt");
    let registry = Registry::with_builtins();

    let meta = Dump {
        storage: &storage,
        source: driver,
        registry: &registry,
        engine: &engine,
        plan: None,
        filters: BTreeMap::new(),
        options: DumpOptions {
            tmp_dir: Some(tmp.path().display().to_string()),
            include_databases: vec![db.name.clone()],
            gzip: true,
            ..Default::default()
        },
    }
    .run(chrono::Utc::now())
    .expect("dump against this server version");

    // Wipe the live database, then restore it from storage: a restore that
    // only ever writes over existing data proves much less.
    driver
        .drop_database(&db.name)
        .expect("drop source database");

    let report = Restore {
        storage: &storage,
        sink: driver,
        exclusions: ErrorExclusions::default(),
        scripts: Default::default(),
        options: RestoreOptions {
            batch_size: 64,
            ..Default::default()
        },
    }
    .run(
        &meta.id,
        &leafmask::restore::ProcessScriptRunner::new(support::uri()),
    )
    .expect("restore against this server version");
    assert_eq!(report.inserted, documents.len() as u64);

    let restored = driver
        .read_collection(&db.name, "users")
        .expect("read restored collection");
    assert_eq!(
        restored.documents.len(),
        documents.len(),
        "document count changed across a dump/restore on MongoDB {:?}",
        server_version()
    );

    let mut restored_docs = restored.documents;
    restored_docs.sort_by_key(|d| d.get_i32("_id").unwrap_or_default());
    assert_eq!(
        restored_docs,
        documents,
        "documents changed across a dump/restore on MongoDB {:?}",
        server_version()
    );
}

/// A sanity check that the collection listing still hides the server's own
/// catalog namespaces — these have moved and been renamed across releases, and
/// dumping one produces a dump that cannot be restored.
#[test]
fn system_namespaces_stay_hidden_on_this_server_version() {
    let db = TestDb::new("system_ns");
    let driver = db.driver();

    driver
        .ensure_collection(&db.name, "real", &None, &BTreeMap::new())
        .expect("create collection");
    driver
        .insert(&db.name, "real", &doc! { "_id": 1i32 })
        .expect("insert");

    let collections = driver.collections(&db.name).expect("list collections");
    assert!(collections.contains(&"real".to_string()));
    for name in &collections {
        assert!(
            !name.starts_with("system."),
            "catalog namespace '{name}' leaked into the dump set on MongoDB {:?}",
            server_version()
        );
    }
}

/// Reading a collection that does not exist must be an empty result, not an
/// error — dump enumerates and reads in two steps, so a collection dropped
/// between them must not abort the run.
#[test]
fn reading_a_missing_collection_is_empty_not_an_error() {
    let db = TestDb::new("missing_coll");
    let driver = db.driver();

    let data: CollectionData = driver
        .read_collection(&db.name, "never_created")
        .expect("reading a missing collection must not error");
    assert!(data.documents.is_empty());
}
