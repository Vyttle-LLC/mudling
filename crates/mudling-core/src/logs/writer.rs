//! One log's way to the disk. The session copies its bytes into a spool in
//! memory, under a lock the writer thread holds only to swap the spool out,
//! never across a write, so a slow disk can't reach the session. The thread
//! sleeps between flushes rather than being woken for each line, as a wake
//! is a system call and a line's budget has no room for one. A full spool
//! drops what doesn't fit, and the log says so.

use super::{Kind, Logs, MOST_BYTES, live};
use crate::persist::Timestamp;
use crate::replay::LOG_NOTE;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// How long lines wait in memory for the disk.
const FLUSH_EVERY: Duration = Duration::from_millis(100);
/// How much waits for the disk: seconds of the fastest text a server sends.
const SPOOL_BYTES: usize = 4 * 1024 * 1024;
/// How much a log grows between looks at the cap.
const PRUNE_EVERY: u64 = 1024 * 1024;

/// What a session should tell the player about a log.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Trouble {
    FellBehind,
    Full,
    Failed(String),
}

#[derive(Default)]
struct Spool {
    bytes: Vec<u8>,
    /// Writes lost since the log last said so.
    dropped: u64,
    /// The first drop has been told.
    told_of_drops: bool,
    /// It won't take any more: the log is full, or its disk failed.
    stopped: bool,
    closing: bool,
    trouble: Option<Trouble>,
}

#[derive(Default)]
struct Shared {
    spool: Mutex<Spool>,
    closing: Condvar,
}

impl Shared {
    fn spool(&self) -> MutexGuard<'_, Spool> {
        self.spool.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn stop(&self, trouble: Trouble) {
        let mut spool = self.spool();
        spool.stopped = true;
        spool.trouble = Some(trouble);
    }
}

/// A log being written. Dropping it ends the log; what's spooled still lands.
pub(super) struct LogWriter {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl LogWriter {
    /// Starts a log, on its own thread, so even making its file can't wait
    /// here.
    pub(super) fn start(logs: &Logs, kind: Kind, host: &str) -> io::Result<LogWriter> {
        let path = logs.new_path(kind, host, Timestamp::now());
        live().insert(path.clone());
        let disk = Disk {
            logs: logs.clone(),
            kind,
            path,
            file: None,
            written: 0,
            pruned_at: 0,
            limit: MOST_BYTES,
        };
        LogWriter::over(disk)
    }

    /// A log written to `sink`, whose [`io::ErrorKind::FileTooLarge`] means
    /// the log is full.
    fn over(sink: impl Write + Send + 'static) -> io::Result<LogWriter> {
        let shared = Arc::new(Shared::default());
        let thread = {
            let shared = Arc::clone(&shared);
            thread::Builder::new()
                .name("mudling-log".to_owned())
                .spawn(move || write_until_closed(&shared, sink))?
        };
        Ok(LogWriter {
            shared,
            thread: Some(thread),
        })
    }

    /// Queues `parts` as one write, or drops it if the spool is full. Returns
    /// any trouble to tell. This never waits on the disk.
    pub(super) fn push(&self, parts: &[&[u8]]) -> Option<Trouble> {
        let mut spool = self.shared.spool();
        let size: usize = parts.iter().map(|part| part.len()).sum();
        if spool.stopped {
            // Nothing more goes in, and that's been said.
        } else if spool.bytes.len() + size > SPOOL_BYTES {
            spool.dropped += 1;
            if !std::mem::replace(&mut spool.told_of_drops, true) {
                return Some(Trouble::FellBehind);
            }
        } else {
            for part in parts {
                spool.bytes.extend_from_slice(part);
            }
        }
        spool.trouble.take()
    }

    /// Ends the log and hands back the thread, still finishing.
    pub(super) fn close(mut self) -> Option<JoinHandle<()>> {
        self.thread.take()
    }
}

impl Drop for LogWriter {
    fn drop(&mut self) {
        self.shared.spool().closing = true;
        self.shared.closing.notify_all();
    }
}

fn write_until_closed(shared: &Shared, mut sink: impl Write) {
    let mut batch = Vec::new();
    loop {
        let (dropped, closing) = {
            let spool = shared.spool();
            let mut spool = if spool.closing {
                spool
            } else {
                shared
                    .closing
                    .wait_timeout(spool, FLUSH_EVERY)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0
            };
            std::mem::swap(&mut spool.bytes, &mut batch);
            (std::mem::take(&mut spool.dropped), spool.closing)
        };
        if dropped > 0 {
            let said = format!("\n{LOG_NOTE}the disk fell behind: {dropped} writes dropped\n");
            batch.extend_from_slice(said.as_bytes());
        }
        if !batch.is_empty()
            && let Err(error) = sink.write_all(&batch)
        {
            shared.stop(match error.kind() {
                io::ErrorKind::FileTooLarge => Trouble::Full,
                _ => Trouble::Failed(error.to_string()),
            });
            return;
        }
        batch.clear();
        if closing {
            return;
        }
    }
}

/// The log's file, made at the first write, keeping its kind to the cap.
struct Disk {
    logs: Logs,
    kind: Kind,
    path: PathBuf,
    file: Option<File>,
    written: u64,
    pruned_at: u64,
    limit: u64,
}

impl Disk {
    fn open(&mut self) -> io::Result<&mut File> {
        if self.file.is_none() {
            if let Some(folder) = self.path.parent() {
                fs::create_dir_all(folder)?;
            }
            self.file = Some(File::create(&self.path)?);
            self.logs.prune(self.kind)?;
        }
        self.file
            .as_mut()
            .ok_or_else(|| io::Error::other("the log closed"))
    }
}

impl Write for Disk {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.written + bytes.len() as u64 > self.limit {
            return Err(io::ErrorKind::FileTooLarge.into());
        }
        let wrote = self.open()?.write(bytes)?;
        self.written += wrote as u64;
        if self.written - self.pruned_at >= PRUNE_EVERY {
            self.pruned_at = self.written;
            self.logs.prune(self.kind)?;
        }
        Ok(wrote)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.as_mut().map_or(Ok(()), Write::flush)
    }
}

impl Drop for Disk {
    fn drop(&mut self) {
        live().remove(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::{self, Receiver, Sender, sync_channel};
    use std::time::Instant;

    /// A disk that takes each write only when the test lets it, and says
    /// when a write has begun waiting.
    struct StuckDisk {
        go: Receiver<()>,
        entered: Sender<()>,
        wrote: Arc<Mutex<Vec<u8>>>,
    }

    impl Write for StuckDisk {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let _ = self.entered.send(());
            let _ = self.go.recv();
            self.wrote.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct FailingDisk(io::ErrorKind);

    impl Write for FailingDisk {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(self.0.into())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn first_trouble(writer: &LogWriter) -> Trouble {
        let started = Instant::now();
        loop {
            if let Some(trouble) = writer.push(&[b"line\n"]) {
                return trouble;
            }
            assert!(started.elapsed() < Duration::from_secs(10), "no trouble");
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_stuck_disk_drops_lines_and_says_so() {
        push_to_a_stuck_disk();
    }

    #[test]
    #[ignore = "timing: runs in the profile"]
    fn timing_a_stuck_disk_doesnt_hold_the_session() {
        assert!(push_to_a_stuck_disk() < Duration::from_secs(2));
    }

    /// Fills a stuck disk's spool twice over; returns how long the pushes took.
    fn push_to_a_stuck_disk() -> Duration {
        let (go, stuck) = sync_channel(0);
        let (entered, writing) = mpsc::channel();
        let wrote = Arc::default();
        let disk = StuckDisk {
            go: stuck,
            entered,
            wrote: Arc::clone(&wrote),
        };
        let writer = LogWriter::over(disk).unwrap();
        let line = [b'x'; 1024];
        // The first flush takes something, then the disk never answers.
        writer.push(&[&line]);
        writing
            .recv_timeout(Duration::from_secs(60))
            .expect("the first flush reaches the disk");

        let started = Instant::now();
        let troubles: Vec<_> = (0..2 * SPOOL_BYTES / line.len())
            .filter_map(|_| writer.push(&[&line]))
            .collect();
        let pushing = started.elapsed();
        assert_eq!(troubles, [Trouble::FellBehind], "said once");

        drop(go);
        writer.close().unwrap().join().unwrap();
        let wrote = String::from_utf8(wrote.lock().unwrap().clone()).unwrap();
        assert!(wrote.contains("writes dropped"), "the log says so too");
        pushing
    }

    #[test]
    fn a_full_log_and_a_failed_disk_each_stop_it_and_say_why() {
        let writer = LogWriter::over(FailingDisk(io::ErrorKind::FileTooLarge)).unwrap();
        assert_eq!(first_trouble(&writer), Trouble::Full);

        let writer = LogWriter::over(FailingDisk(io::ErrorKind::PermissionDenied)).unwrap();
        assert!(matches!(first_trouble(&writer), Trouble::Failed(_)));
    }

    #[test]
    fn what_was_spooled_lands_when_the_log_closes() {
        let folder = tempfile::tempdir().unwrap();
        let logs = Logs::in_data_folder(folder.path());
        let writer = LogWriter::start(&logs, Kind::Debug, "mud.example").unwrap();
        assert!(writer.push(&[b"one ", b"line\n"]).is_none());
        writer.close().unwrap().join().unwrap();

        let log = logs.list().unwrap().remove(0);
        assert_eq!(log.server, "mud.example");
        assert!(!log.live, "it's over, so it can go");
        assert_eq!(std::fs::read(&log.path).unwrap(), b"one line\n");
    }

    #[test]
    fn a_log_stops_at_its_limit() {
        let folder = tempfile::tempdir().unwrap();
        let logs = Logs::in_data_folder(folder.path());
        let path = logs.new_path(Kind::Debug, "mud.example", Timestamp(1));
        let disk = Disk {
            logs,
            kind: Kind::Debug,
            path,
            file: None,
            written: 0,
            pruned_at: 0,
            limit: 4,
        };
        let writer = LogWriter::over(disk).unwrap();
        assert_eq!(first_trouble(&writer), Trouble::Full);
    }
}
