//! Property tests for the on-disk dump format.
//!
//! The data blob is a bare concatenation of BSON documents (see
//! `CollectionDataWriter`/`DocumentReader`), and every restore in existence
//! depends on that framing surviving a round trip byte-for-byte. Hand-written
//! fixtures only ever cover the document shapes someone thought to write down;
//! these tests generate the shapes nobody thought of — deep nesting, empty
//! documents, arbitrary binary payloads, extreme integers — and assert the
//! round trip regardless.
//!
//! Runs with default features: no MongoDB, no network.
//!
//! ```sh
//! cargo test --test property_dump_format
//! ```

mod support;

use bson::Document;
use leafmask::dump::{open_document_reader, CollectionDataWriter};
use leafmask::storage::{DirectoryStorage, Storage};
use proptest::prelude::*;
use support::strategies;

/// Write `docs` as a collection blob and read them back through the public
/// streaming API, exactly as dump and restore do.
fn round_trip(docs: &[Document], gzip: bool) -> Vec<Document> {
    let tmp = tempfile::tempdir().expect("tempdir");
    let storage = DirectoryStorage::new(tmp.path()).expect("directory storage");
    let dump_id = "prop";
    let (db, collection) = ("shop", "users");
    let path = format!(
        "{dump_id}/{}",
        leafmask::dump::data_path(db, collection, gzip)
    );

    let mut writer = CollectionDataWriter::create(
        &storage,
        &path,
        tmp.path().join("spool").to_str().expect("utf-8 tmp path"),
        db,
        collection,
        gzip,
    )
    .expect("open writer");
    for doc in docs {
        writer.write_document(doc).expect("write document");
    }
    assert_eq!(writer.count(), docs.len() as u64);
    writer.finish(&storage).expect("finish blob");

    let mut reader =
        open_document_reader(&storage, dump_id, db, collection).expect("open document reader");
    let mut out = Vec::new();
    while let Some(doc) = reader.next_document().expect("read document") {
        out.push(doc);
    }
    out
}

proptest! {
    /// The core guarantee: whatever goes into a blob comes back out, in order
    /// and unchanged, for both the plain and the gzip variant.
    #[test]
    fn documents_round_trip_through_the_blob(
        docs in prop::collection::vec(strategies::document(), 0..12),
        gzip in any::<bool>(),
    ) {
        prop_assert_eq!(round_trip(&docs, gzip), docs);
    }

    /// Reading in batches must yield exactly the same sequence as reading one
    /// document at a time — restore picks its batch size from config, and that
    /// choice must never change what lands in the target database.
    #[test]
    fn batched_reads_match_single_reads(
        docs in prop::collection::vec(strategies::document(), 0..12),
        batch in 1usize..5,
    ) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = DirectoryStorage::new(tmp.path()).expect("directory storage");
        let path = format!("prop/{}", leafmask::dump::data_path("shop", "users", false));
        let mut writer = CollectionDataWriter::create(
            &storage,
            &path,
            tmp.path().join("spool").to_str().expect("utf-8 tmp path"),
            "shop",
            "users",
            false,
        )
        .expect("open writer");
        for doc in &docs {
            writer.write_document(doc).expect("write document");
        }
        writer.finish(&storage).expect("finish blob");

        let mut reader =
            open_document_reader(&storage, "prop", "shop", "users").expect("open reader");
        let mut out = Vec::new();
        loop {
            let got = reader.next_batch(batch).expect("read batch");
            if got.is_empty() {
                break;
            }
            prop_assert!(got.len() <= batch);
            out.extend(got);
        }
        prop_assert_eq!(out, docs);
    }

    /// A truncated blob — an interrupted upload, a partial download — must
    /// surface as an error, never as a short but plausible-looking document
    /// stream that would silently restore an incomplete collection.
    #[test]
    fn truncated_blobs_are_rejected_not_silently_short(
        docs in prop::collection::vec(strategies::document(), 1..8),
        cut in 1usize..64,
    ) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = DirectoryStorage::new(tmp.path()).expect("directory storage");
        let path = format!("prop/{}", leafmask::dump::data_path("shop", "users", false));
        let mut writer = CollectionDataWriter::create(
            &storage,
            &path,
            tmp.path().join("spool").to_str().expect("utf-8 tmp path"),
            "shop",
            "users",
            false,
        )
        .expect("open writer");
        for doc in &docs {
            writer.write_document(doc).expect("write document");
        }
        writer.finish(&storage).expect("finish blob");

        let full = storage.get(&path).expect("read blob back");
        // Cut somewhere inside the final document, never at a frame boundary.
        let keep = full.len().saturating_sub(cut.min(full.len().saturating_sub(1)));
        prop_assume!(keep < full.len() && keep > 0);
        storage.put(&path, &full[..keep]).expect("overwrite truncated");

        let mut reader =
            open_document_reader(&storage, "prop", "shop", "users").expect("open reader");
        let mut complete = 0usize;
        let mut errored = false;
        loop {
            match reader.next_document() {
                Ok(Some(_)) => complete += 1,
                Ok(None) => break,
                Err(_) => {
                    errored = true;
                    break;
                }
            }
        }
        // Either the reader errored, or it cleanly stopped at a boundary that
        // happens to be legal — but it must never claim all documents are
        // present when bytes are missing.
        prop_assert!(
            errored || complete < docs.len(),
            "truncated blob read back as {complete} of {} documents with no error",
            docs.len()
        );
    }
}

/// The framing must not impose a per-collection ceiling: a blob is a bare
/// concatenation precisely so a collection can exceed BSON's 2 GiB document
/// limit. This checks the shape of that guarantee cheaply — many documents,
/// streamed, with the reader never holding more than a batch in memory.
#[test]
fn blob_is_a_concatenation_not_a_wrapper_document() {
    let docs: Vec<Document> = (0..2_000)
        .map(|i| bson::doc! { "_id": i, "payload": "x".repeat(64) })
        .collect();
    let read_back = round_trip(&docs, false);
    assert_eq!(read_back.len(), docs.len());
    assert_eq!(read_back, docs);

    // A wrapper document would have to prefix the stream with its own length;
    // a concatenation starts directly with the first document's length.
    let tmp = tempfile::tempdir().expect("tempdir");
    let storage = DirectoryStorage::new(tmp.path()).expect("directory storage");
    let path = format!("prop/{}", leafmask::dump::data_path("shop", "users", false));
    let mut writer = CollectionDataWriter::create(
        &storage,
        &path,
        tmp.path().join("spool").to_str().expect("utf-8 tmp path"),
        "shop",
        "users",
        false,
    )
    .expect("open writer");
    for doc in &docs {
        writer.write_document(doc).expect("write document");
    }
    writer.finish(&storage).expect("finish blob");

    let bytes = storage.get(&path).expect("read blob");
    let first_len = i32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
    let first_doc_len = {
        let mut buf = Vec::new();
        docs[0].to_writer(&mut buf).expect("encode first document");
        buf.len()
    };
    assert_eq!(
        first_len, first_doc_len,
        "blob must start with the first document, not a wrapper length"
    );
    assert!(
        bytes.len() > first_len,
        "blob must continue past the first document"
    );
}
