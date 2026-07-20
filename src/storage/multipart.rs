//! Generic streaming multipart upload machinery shared by the S3 and Azure
//! backends: buffers writes into adaptively-sized parts and hands each full
//! part to a dedicated background thread, so the upload overlaps with
//! whatever is still being written instead of only starting once the whole
//! blob is ready.

use std::cell::RefCell;
use std::io::{self, Write};
use std::sync::mpsc::{self, SyncSender};
use std::thread::{self, JoinHandle};

use crate::error::{Error, Result};

use super::MultipartWriter;

/// Starting part size: comfortably above S3's 5 MiB per-part minimum, small
/// enough to give several parts (real overlap, and several log lines)
/// even on modest collections.
pub(crate) const INITIAL_PART_SIZE: usize = 8 * 1024 * 1024;

/// How many parts to send at the current size before doubling it. S3 caps a
/// single multipart upload at 10,000 parts; doubling on this cadence keeps
/// even a very large collection well under that ceiling without needing to
/// know the total size upfront. See the design doc for the worked-out size
/// table.
const PARTS_PER_DOUBLING: u64 = 1000;

/// Tracks the adaptive part-size schedule purely from how many parts have
/// been sent so far — no upfront size estimate needed.
pub(crate) struct PartSizeSchedule {
    parts_sent: u64,
}

impl PartSizeSchedule {
    pub(crate) fn new() -> Self {
        PartSizeSchedule { parts_sent: 0 }
    }

    pub(crate) fn current(&self) -> usize {
        let doublings = (self.parts_sent / PARTS_PER_DOUBLING).min(24);
        INITIAL_PART_SIZE << doublings
    }

    pub(crate) fn record_part_sent(&mut self) {
        self.parts_sent += 1;
    }

    pub(crate) fn parts_sent(&self) -> u64 {
        self.parts_sent
    }
}

/// One message on the channel between the writing thread and the background
/// upload thread.
pub(crate) enum Msg {
    Data(Vec<u8>),
    Finish,
    Abort,
}

/// What a backend implements to plug into [`StreamingMultipartWriter`]: how
/// to send one part/block, how to complete the upload once every part is
/// acknowledged, and how to cancel it. Runs entirely on the background
/// thread `StreamingMultipartWriter::spawn` creates.
pub(crate) trait PartSink: Send {
    /// Upload part number `part_number` (1-based, sequential). Returns an
    /// opaque token needed at completion time (an S3 ETag, an Azure block
    /// ID, ...).
    fn send_part(&mut self, part_number: u64, data: Vec<u8>) -> Result<String>;
    /// Complete the upload from the tokens returned by `send_part`, in the
    /// order the parts were sent.
    fn complete(self: Box<Self>, tokens: Vec<String>) -> Result<()>;
    /// Best-effort cancellation of whatever was started server-side.
    fn abort(&mut self);
}

fn run_uploader(mut sink: Box<dyn PartSink>, rx: mpsc::Receiver<Msg>) -> Result<()> {
    let mut part_number = 1u64;
    let mut tokens = Vec::new();
    loop {
        match rx.recv() {
            Ok(Msg::Data(data)) => match sink.send_part(part_number, data) {
                Ok(token) => {
                    tokens.push(token);
                    part_number += 1;
                }
                Err(e) => {
                    sink.abort();
                    return Err(e);
                }
            },
            Ok(Msg::Finish) => return sink.complete(tokens),
            Ok(Msg::Abort) | Err(_) => {
                sink.abort();
                return Ok(());
            }
        }
    }
}

/// A [`MultipartWriter`] that buffers writes per [`PartSizeSchedule`] and
/// uploads each completed part on a dedicated background thread, via a
/// [`PartSink`].
pub(crate) struct StreamingMultipartWriter {
    buffer: Vec<u8>,
    schedule: PartSizeSchedule,
    total_bytes: u64,
    tx: RefCell<Option<SyncSender<Msg>>>,
    join: RefCell<Option<JoinHandle<Result<()>>>>,
}

impl StreamingMultipartWriter {
    pub(crate) fn spawn(sink: Box<dyn PartSink>) -> Self {
        let (tx, rx) = mpsc::sync_channel(2);
        let join = thread::spawn(move || run_uploader(sink, rx));
        StreamingMultipartWriter {
            buffer: Vec::new(),
            schedule: PartSizeSchedule::new(),
            total_bytes: 0,
            tx: RefCell::new(Some(tx)),
            join: RefCell::new(Some(join)),
        }
    }

    fn send(&self, part: Vec<u8>) -> io::Result<()> {
        let tx = self.tx.borrow();
        match tx.as_ref() {
            Some(tx) => tx
                .send(Msg::Data(part))
                .map_err(|_| io::Error::other("multipart upload thread ended early")),
            None => Err(io::Error::other("multipart writer already finished")),
        }
    }
}

impl Write for StreamingMultipartWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buffer.extend_from_slice(buf);
        self.total_bytes += buf.len() as u64;
        loop {
            let part_size = self.schedule.current();
            if self.buffer.len() < part_size {
                break;
            }
            let part: Vec<u8> = self.buffer.drain(..part_size).collect();
            self.send(part)?;
            self.schedule.record_part_sent();
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl MultipartWriter for StreamingMultipartWriter {
    fn finish(mut self: Box<Self>) -> Result<u64> {
        let last = std::mem::take(&mut self.buffer);
        // Always send at least one part, even an empty one, so the
        // completion call is never given zero parts (both S3 and Azure
        // require at least one) — this covers a genuinely empty collection.
        if !last.is_empty() || self.schedule.parts_sent() == 0 {
            // Best-effort: if the channel is already disconnected, the
            // background thread has already exited (most likely from a part
            // failure) — ignore this error and let join() below surface the
            // real underlying cause instead of this generic one.
            let _ = self.send(last);
        }
        if let Some(tx) = self.tx.borrow_mut().take() {
            let _ = tx.send(Msg::Finish);
        }
        if let Some(handle) = self.join.borrow_mut().take() {
            handle
                .join()
                .map_err(|_| Error::Storage("multipart upload thread panicked".into()))??;
        }
        Ok(self.total_bytes)
    }

    fn abort(&self) {
        if let Some(tx) = self.tx.borrow_mut().take() {
            let _ = tx.send(Msg::Abort);
        }
        if let Some(handle) = self.join.borrow_mut().take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    type PartList = Arc<Mutex<Vec<(u64, Vec<u8>)>>>;

    /// Records every call so tests can assert on behavior without any
    /// network access.
    struct FakeSink {
        parts: PartList,
        completed: Arc<Mutex<Option<Vec<String>>>>,
        aborted: Arc<Mutex<bool>>,
        fail_on_part: Option<u64>,
    }

    impl PartSink for FakeSink {
        fn send_part(&mut self, part_number: u64, data: Vec<u8>) -> Result<String> {
            if self.fail_on_part == Some(part_number) {
                return Err(Error::Storage("simulated part failure".into()));
            }
            self.parts.lock().unwrap().push((part_number, data));
            Ok(format!("token-{part_number}"))
        }

        fn complete(self: Box<Self>, tokens: Vec<String>) -> Result<()> {
            *self.completed.lock().unwrap() = Some(tokens);
            Ok(())
        }

        fn abort(&mut self) {
            *self.aborted.lock().unwrap() = true;
        }
    }

    struct Fixture {
        parts: PartList,
        completed: Arc<Mutex<Option<Vec<String>>>>,
        aborted: Arc<Mutex<bool>>,
    }

    fn fixture(fail_on_part: Option<u64>) -> (Fixture, Box<dyn PartSink>) {
        let parts = Arc::new(Mutex::new(Vec::new()));
        let completed = Arc::new(Mutex::new(None));
        let aborted = Arc::new(Mutex::new(false));
        let sink: Box<dyn PartSink> = Box::new(FakeSink {
            parts: parts.clone(),
            completed: completed.clone(),
            aborted: aborted.clone(),
            fail_on_part,
        });
        (
            Fixture {
                parts,
                completed,
                aborted,
            },
            sink,
        )
    }

    // Writes smaller than one part stay buffered until finish(), which must
    // still send exactly one (possibly small) part — required so the
    // completion call is never given zero parts.
    #[test]
    fn small_write_sends_one_part_on_finish() {
        let (f, sink) = fixture(None);
        let mut writer: Box<dyn MultipartWriter> = Box::new(StreamingMultipartWriter::spawn(sink));
        writer.write_all(b"hello").unwrap();
        let total = writer.finish().unwrap();

        assert_eq!(total, 5);
        assert_eq!(
            f.parts.lock().unwrap().as_slice(),
            &[(1, b"hello".to_vec())]
        );
        assert_eq!(
            f.completed.lock().unwrap().as_deref(),
            Some(&["token-1".to_string()][..])
        );
        assert!(!*f.aborted.lock().unwrap());
    }

    // A write crossing part-size boundaries sends one part per boundary as
    // it's written, before finish() is ever called — this is the actual
    // overlap behavior (parts upload while more data keeps arriving).
    #[test]
    fn write_crossing_part_boundary_sends_parts_eagerly() {
        let (f, sink) = fixture(None);
        let mut writer: Box<dyn MultipartWriter> = Box::new(StreamingMultipartWriter::spawn(sink));
        let chunk = vec![b'x'; INITIAL_PART_SIZE];
        writer.write_all(&chunk).unwrap();
        writer.write_all(&chunk).unwrap();
        writer.write_all(b"tail").unwrap();
        let total = writer.finish().unwrap();

        assert_eq!(total as usize, INITIAL_PART_SIZE * 2 + 4);
        let sent = f.parts.lock().unwrap();
        assert_eq!(sent.len(), 3);
        assert_eq!(sent[0].0, 1);
        assert_eq!(sent[0].1.len(), INITIAL_PART_SIZE);
        assert_eq!(sent[2].1, b"tail");
        assert_eq!(
            f.completed.lock().unwrap().as_deref(),
            Some(
                &[
                    "token-1".to_string(),
                    "token-2".to_string(),
                    "token-3".to_string()
                ][..]
            )
        );
    }

    // The adaptive schedule doubles every 1000 parts, never needing to know
    // the total size upfront — this is what keeps a full dump of a very
    // large collection under S3's 10,000-part ceiling.
    #[test]
    fn part_size_doubles_every_thousand_parts() {
        let mut schedule = PartSizeSchedule::new();
        assert_eq!(schedule.current(), INITIAL_PART_SIZE);
        for _ in 0..1000 {
            schedule.record_part_sent();
        }
        assert_eq!(schedule.current(), INITIAL_PART_SIZE * 2);
        for _ in 0..1000 {
            schedule.record_part_sent();
        }
        assert_eq!(schedule.current(), INITIAL_PART_SIZE * 4);
    }

    // abort() cancels instead of completing, and is safe to call without
    // ever writing anything — this is CollectionDataWriter's Drop path on
    // an early error.
    #[test]
    fn abort_cancels_without_completing() {
        let (f, sink) = fixture(None);
        let writer: Box<dyn MultipartWriter> = Box::new(StreamingMultipartWriter::spawn(sink));
        writer.abort();

        assert!(*f.aborted.lock().unwrap());
        assert!(f.completed.lock().unwrap().is_none());
    }

    // A failure from the background sink surfaces through finish(), not
    // silently, and triggers an abort — the caller (CollectionDataWriter)
    // must see the real error, and nothing is left dangling server-side.
    #[test]
    fn part_failure_surfaces_from_finish_and_aborts() {
        let (f, sink) = fixture(Some(1));
        let mut writer: Box<dyn MultipartWriter> = Box::new(StreamingMultipartWriter::spawn(sink));
        // Force one full part to be sent eagerly, so the failure happens on
        // the background thread before finish() is even called.
        writer.write_all(&vec![b'x'; INITIAL_PART_SIZE]).unwrap();
        // Give the background thread a moment to process and fail.
        std::thread::sleep(std::time::Duration::from_millis(50));

        let result = writer.finish();
        assert!(
            result.is_err(),
            "expected finish() to surface the part failure"
        );
        assert!(*f.aborted.lock().unwrap());
    }

    // The literal "collection was completely empty" case: finish() must still
    // send exactly one (empty) part when write() was never called at all —
    // this is the scenario the "at least one part" requirement exists for
    // (both S3 and Azure require at least one part/block to complete an
    // upload).
    #[test]
    fn finish_without_any_write_sends_one_empty_part() {
        let (f, sink) = fixture(None);
        let writer: Box<dyn MultipartWriter> = Box::new(StreamingMultipartWriter::spawn(sink));
        let total = writer.finish().unwrap();

        assert_eq!(total, 0);
        assert_eq!(f.parts.lock().unwrap().as_slice(), &[(1, Vec::new())]);
        assert_eq!(
            f.completed.lock().unwrap().as_deref(),
            Some(&["token-1".to_string()][..])
        );
    }

    // Regression test for the finish() error-masking fix above: when the
    // background thread has already died from an earlier part failure, and
    // finish() still has a non-empty trailing buffer to send, the real
    // underlying error must surface — not a generic "thread ended early"
    // message from the failed trailing send.
    #[test]
    fn finish_surfaces_real_error_even_when_trailing_send_fails() {
        let (f, sink) = fixture(Some(1));
        let mut writer: Box<dyn MultipartWriter> = Box::new(StreamingMultipartWriter::spawn(sink));
        // One full part (fails on the background thread) plus a non-empty
        // trailing remainder, so finish() has real bytes left to send after
        // the background thread has already exited.
        writer.write_all(&vec![b'x'; INITIAL_PART_SIZE]).unwrap();
        writer.write_all(b"tail").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));

        let err = writer.finish().unwrap_err();
        assert!(
            err.to_string().contains("simulated part failure"),
            "expected the real sink error, got: {err}"
        );
        assert!(*f.aborted.lock().unwrap());
    }
}
