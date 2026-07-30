//! Shared helpers for the integration test suites.
//!
//! Included by each `tests/*.rs` target with `mod support;`. Cargo compiles
//! every top-level file in `tests/` as its own crate, so any given helper is
//! unused in most of them — hence the blanket `dead_code` allowance.

#![allow(dead_code)]

use bson::{Bson, Document};

// ---------------------------------------------------------------------------
// MongoDB server versions under test
// ---------------------------------------------------------------------------

/// Server releases Leafmask is tested against, oldest first.
///
/// CI runs the whole `mongo`-gated suite once per entry (see the `mongo`
/// matrix in `.github/workflows/ci.yml`), and `scripts/test-mongo-matrix.sh`
/// reproduces that locally. Adding a future release is a one-line change here
/// plus the same line in the workflow matrix: every existing test then has to
/// pass against it, which is what makes a new server version a *reviewable*
/// event rather than a silent one.
pub const SUPPORTED_SERVER_VERSIONS: &[(u32, u32)] = &[(6, 0), (7, 0), (8, 0)];

/// The MongoDB URI under test (`LEAFMASK_MONGO_URI`, default local).
pub fn uri() -> String {
    std::env::var("LEAFMASK_MONGO_URI").unwrap_or_else(|_| "mongodb://localhost:27017".into())
}

/// A unique database name per test, so parallel tests never collide.
pub fn db_name(tag: &str) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("leafmask_it_{tag}_{}_{}", std::process::id(), nanos)
}

#[cfg(feature = "mongo")]
mod mongo_support {
    use super::*;
    use leafmask::mongo::MongoDriver;

    /// Connect to the MongoDB under test.
    pub fn connect() -> MongoDriver {
        MongoDriver::connect(&uri()).expect("connect to MongoDB (is the container running?)")
    }

    /// The connected server's `(major, minor, patch)`.
    ///
    /// Cached: it appears in assertion messages all over the matrix suite, and
    /// every `MongoDriver` spins up its own Tokio runtime to connect.
    pub fn server_version() -> (u32, u32, u32) {
        static VERSION: std::sync::OnceLock<(u32, u32, u32)> = std::sync::OnceLock::new();
        *VERSION.get_or_init(|| {
            connect()
                .server_version()
                .expect("read server version via buildInfo")
        })
    }

    /// True when the server under test is at least `(major, minor)`.
    ///
    /// Use it to *gate assertions*, never to skip an entire test: a test that
    /// silently vanishes on a new server version is exactly the regression the
    /// matrix exists to catch.
    pub fn server_at_least(major: u32, minor: u32) -> bool {
        let (svr_major, svr_minor, _) = server_version();
        (svr_major, svr_minor) >= (major, minor)
    }

    /// A test database that is dropped when it goes out of scope — including
    /// on panic, so a failing assertion never leaves residue behind for the
    /// next matrix leg to trip over.
    pub struct TestDb {
        pub name: String,
        driver: Option<MongoDriver>,
    }

    impl TestDb {
        pub fn new(tag: &str) -> Self {
            let name = db_name(tag);
            let driver = connect();
            // A previous crashed run could have left this name behind only if
            // the pid and nanosecond clock repeated; dropping first is cheap
            // insurance and keeps the test's starting state unambiguous.
            let _ = driver.drop_database(&name);
            TestDb {
                name,
                driver: Some(driver),
            }
        }

        pub fn driver(&self) -> &MongoDriver {
            self.driver.as_ref().expect("driver still held")
        }
    }

    impl Drop for TestDb {
        fn drop(&mut self) {
            if let Some(driver) = self.driver.take() {
                let _ = driver.drop_database(&self.name);
            }
        }
    }
}

#[cfg(feature = "mongo")]
#[allow(unused_imports)]
pub use mongo_support::{connect, server_at_least, server_version, TestDb};

// ---------------------------------------------------------------------------
// Property-test strategies
// ---------------------------------------------------------------------------

/// Strategies producing arbitrary-but-legal BSON, used by the property tests
/// to exercise the dump framing and the transformer registry against value
/// shapes no hand-written fixture covers.
pub mod strategies {
    use super::*;
    use proptest::prelude::*;

    /// A BSON scalar, covering every type Leafmask can write to a dump —
    /// deliberately including Decimal128, Timestamp and RegularExpression,
    /// which `mongo_version_matrix.rs` proves survive a real server round trip
    /// and which must therefore survive the dump format too.
    ///
    /// Excluded on purpose: JavaScript (with and without scope), DBPointer,
    /// Symbol, Undefined, and the min/max keys — deprecated or internal types
    /// that no deployment produces for ordinary user data.
    pub fn scalar() -> impl Strategy<Value = Bson> {
        prop_oneof![
            Just(Bson::Null),
            any::<bool>().prop_map(Bson::Boolean),
            any::<i32>().prop_map(Bson::Int32),
            any::<i64>().prop_map(Bson::Int64),
            // NaN and the infinities are legal BSON doubles but are not equal
            // to themselves, which would make round-trip equality meaningless.
            any::<f64>()
                .prop_filter("finite", |f| f.is_finite())
                .prop_map(Bson::Double),
            ".*".prop_map(Bson::String),
            prop::collection::vec(any::<u8>(), 0..64).prop_map(|bytes| Bson::Binary(
                bson::Binary {
                    subtype: bson::spec::BinarySubtype::Generic,
                    bytes,
                }
            )),
            // Dates are millisecond-precise; generate whole milliseconds within
            // the range chrono can represent so equality stays exact.
            (-62_135_596_800_000i64..253_402_300_799_000i64)
                .prop_map(|ms| Bson::DateTime(bson::DateTime::from_millis(ms))),
            any::<[u8; 12]>().prop_map(|b| Bson::ObjectId(bson::oid::ObjectId::from_bytes(b))),
            (any::<u32>(), any::<u32>())
                .prop_map(|(time, increment)| Bson::Timestamp(bson::Timestamp { time, increment })),
            // Built from an integer literal: an arbitrary 16-byte pattern can
            // decode to Decimal128 NaN, which (like f64 NaN) is not equal to
            // itself and would make round-trip equality meaningless.
            any::<i64>().prop_map(|n| Bson::Decimal128(
                n.to_string().parse().expect("integer parses as decimal128")
            )),
            // Options must be sorted and unique — BSON stores them as written,
            // but MongoDB only ever emits them in canonical order.
            ("[a-z ]{0,12}", prop::collection::btree_set("[imsx]", 0..4)).prop_map(
                |(pattern, options)| Bson::RegularExpression(bson::Regex {
                    pattern,
                    options: options.into_iter().collect::<Vec<_>>().concat(),
                })
            ),
        ]
    }

    /// A field name valid inside a BSON document: non-empty, no NUL byte, and
    /// no leading `$` (which MongoDB reserves for operators).
    pub fn field_name() -> impl Strategy<Value = String> {
        "[a-zA-Z_][a-zA-Z0-9_]{0,15}".prop_map(String::from)
    }

    /// An arbitrary BSON value, nesting documents and arrays a few levels deep.
    pub fn value() -> impl Strategy<Value = Bson> {
        scalar().prop_recursive(3, 24, 5, |inner| {
            prop_oneof![
                prop::collection::vec(inner.clone(), 0..5).prop_map(Bson::Array),
                prop::collection::vec((field_name(), inner), 0..5).prop_map(|pairs| {
                    let mut doc = Document::new();
                    for (k, v) in pairs {
                        doc.insert(k, v);
                    }
                    Bson::Document(doc)
                }),
            ]
        })
    }

    /// An arbitrary BSON document.
    pub fn document() -> impl Strategy<Value = Document> {
        prop::collection::vec((field_name(), value()), 0..8).prop_map(|pairs| {
            let mut doc = Document::new();
            for (k, v) in pairs {
                doc.insert(k, v);
            }
            doc
        })
    }
}
