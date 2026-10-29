//! The writer thread: the only place MUDling writes to a server's database.

use super::Error;
use super::batch::Batch;
use rusqlite::{Connection, TransactionBehavior};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::thread::{self, JoinHandle};

/// Takes batches from any thread without waiting on the disk; its thread
/// commits each batch in one transaction, in the order they came. Dropping
/// it commits whatever is still queued and waits for that, so drop it on a
/// core thread (or at quit), never on the UI thread mid-session.
pub struct Writer {
    sender: Option<Sender<Message>>,
    thread: Option<JoinHandle<()>>,
}

enum Message {
    Batch(Batch),
    Flush(SyncSender<Vec<rusqlite::Error>>),
}

impl Writer {
    pub(crate) fn spawn(connection: Connection) -> Result<Writer, Error> {
        let (sender, receiver) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("mudling-persist".to_owned())
            .spawn(move || run(connection, receiver))
            .map_err(Error::Io)?;
        Ok(Writer {
            sender: Some(sender),
            thread: Some(thread),
        })
    }

    /// Queues a batch. This never touches the disk; it fails only if the
    /// writer thread has died.
    pub fn submit(&self, batch: Batch) -> Result<(), Error> {
        if batch.is_empty() {
            return Ok(());
        }
        self.send(Message::Batch(batch))
    }

    /// Waits until every batch queued so far is committed or has failed,
    /// and reports the batches that failed since the last flush. This
    /// blocks on the disk: for tests, benchmarks and shutdown.
    pub fn flush(&self) -> Result<(), Error> {
        let (reply, failures) = mpsc::sync_channel(1);
        self.send(Message::Flush(reply))?;
        let failures = failures.recv().map_err(|_| Error::WriterStopped)?;
        if failures.is_empty() {
            Ok(())
        } else {
            Err(Error::BatchesFailed(failures))
        }
    }

    fn send(&self, message: Message) -> Result<(), Error> {
        let sender = self.sender.as_ref().expect("the sender lives until drop");
        sender.send(message).map_err(|_| Error::WriterStopped)
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        // Closing the channel lets the thread drain the queue, then stop.
        self.sender.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run(mut connection: Connection, receiver: Receiver<Message>) {
    let mut failures = Vec::new();
    for message in receiver {
        match message {
            Message::Batch(batch) => {
                if let Err(error) = commit(&mut connection, &batch) {
                    failures.push(error);
                }
            }
            Message::Flush(reply) => {
                let _ = reply.send(std::mem::take(&mut failures));
            }
        }
    }
}

fn commit(connection: &mut Connection, batch: &Batch) -> rusqlite::Result<()> {
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    for change in &batch.changes {
        change.apply(&tx)?;
    }
    // A failed commit (a dangling foreign key) rolls back when `tx` drops.
    tx.commit()
}
