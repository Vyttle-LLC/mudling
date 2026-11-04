//! The socket to a server: TCP through tokio, and nothing else.
//!
//! [`connect`] spawns the connection on a core runtime and returns at once,
//! so any thread can call it. The [`Connection`] handle queues bytes without
//! blocking; what the socket does arrives on a channel, starting with
//! [`Event::Connected`] and ending with [`Event::Closed`].
//!
//! Everything past the socket read happens elsewhere, on the bytes of
//! [`Event::Received`], so a recorded byte stream takes the same path as a
//! live one (see [`crate::session`]).

use bytes::{Bytes, BytesMut};
use std::fmt;
use std::io;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::{Instant, sleep_until};

pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the server stays silent before [`Event::Quiet`]: longer than the
/// gap between two reads of one burst, short enough that a prompt with no GA
/// shows at once.
const QUIET_AFTER: Duration = Duration::from_millis(250);

/// A fresh read buffer's size, allocated when the current one has less than
/// `MIN_READ` left. It is reclaimed in place once the slices cut from it have
/// been dropped.
const READ_SIZE: usize = 64 * 1024;
const MIN_READ: usize = 4 * 1024;
const EVENT_QUEUE: usize = 1024;

/// What happened on a socket.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// The TCP connection is open.
    Connected,
    /// The next bytes the server sent, in any chunking.
    Received(Bytes),
    /// The server has said nothing for a moment since its last read. Text
    /// left unended by now is likely a prompt, on a server that marks prompts
    /// with neither GA nor GMCP.
    Quiet,
    /// The connection closed.
    Closed(DisconnectReason),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DisconnectReason {
    /// [`Connection::close`], or every handle dropped: a cancel, whether the
    /// socket was open yet or not.
    ClosedByClient,
    ClosedByServer,
    Failed(String),
}

impl fmt::Display for DisconnectReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DisconnectReason::ClosedByClient => f.write_str("closed by client"),
            DisconnectReason::ClosedByServer => f.write_str("closed by server"),
            DisconnectReason::Failed(message) => write!(f, "failed: {message}"),
        }
    }
}

pub struct ConnectOptions {
    pub host: String,
    pub port: u16,
    /// How long to wait for the TCP connection, usually
    /// [`DEFAULT_CONNECT_TIMEOUT`].
    pub connect_timeout: Duration,
}

/// Writes to the server. Clones share the connection; it closes when
/// [`Connection::close`] is called or every clone is dropped. Writes after
/// the connection closed are dropped.
#[derive(Clone, Debug)]
pub struct Connection {
    commands: mpsc::UnboundedSender<Command>,
}

#[derive(Debug)]
enum Command {
    Write(Vec<u8>),
    Close,
}

impl Connection {
    /// Queues bytes for the socket, already telnet-encoded.
    pub fn write(&self, bytes: Vec<u8>) {
        let _ = self.commands.send(Command::Write(bytes));
    }

    pub fn close(&self) {
        let _ = self.commands.send(Command::Close);
    }
}

/// Connects on `runtime`, which must have I/O and timers enabled.
pub fn connect(
    runtime: &tokio::runtime::Handle,
    options: ConnectOptions,
) -> (Connection, mpsc::Receiver<Event>) {
    let (commands, command_queue) = mpsc::unbounded_channel();
    let (event_queue, events) = mpsc::channel(EVENT_QUEUE);
    runtime.spawn(async move {
        let reason = run(options, command_queue, &event_queue)
            .await
            .unwrap_or_else(|error| DisconnectReason::Failed(error.to_string()));
        let _ = event_queue.send(Event::Closed(reason)).await;
    });
    (Connection { commands }, events)
}

async fn run(
    options: ConnectOptions,
    mut commands: mpsc::UnboundedReceiver<Command>,
    events: &mpsc::Sender<Event>,
) -> io::Result<DisconnectReason> {
    let mut out = Vec::new();
    let ConnectOptions {
        host,
        port,
        connect_timeout,
    } = options;
    let stream = match open(&host, port, connect_timeout, &mut commands, &mut out).await? {
        Ok(stream) => stream,
        Err(reason) => return Ok(reason),
    };
    stream.set_nodelay(true)?;
    if events.send(Event::Connected).await.is_err() {
        return Ok(DisconnectReason::ClosedByClient);
    }
    let (mut reader, mut writer) = stream.into_split();
    writer.write_all(&out).await?;
    let mut buffer = BytesMut::new();
    let quiet = sleep_until(Instant::now());
    tokio::pin!(quiet);
    let mut awaiting_quiet = false;
    loop {
        if buffer.capacity() < MIN_READ {
            buffer.reserve(READ_SIZE);
        }
        tokio::select! {
            read = reader.read_buf(&mut buffer) => {
                if read? == 0 {
                    return Ok(DisconnectReason::ClosedByServer);
                }
                if events.send(Event::Received(buffer.split().freeze())).await.is_err() {
                    return Ok(DisconnectReason::ClosedByClient);
                }
                quiet.as_mut().reset(Instant::now() + QUIET_AFTER);
                awaiting_quiet = true;
            }
            () = &mut quiet, if awaiting_quiet => {
                awaiting_quiet = false;
                if events.send(Event::Quiet).await.is_err() {
                    return Ok(DisconnectReason::ClosedByClient);
                }
            }
            command = commands.recv() => match command {
                Some(Command::Write(bytes)) => writer.write_all(&bytes).await?,
                Some(Command::Close) | None => {
                    let _ = writer.shutdown().await;
                    return Ok(DisconnectReason::ClosedByClient);
                }
            }
        }
    }
}

/// Opens the TCP connection within `timeout`. Bytes written meanwhile wait in
/// `out`; a close, or every handle dropped, cancels the connect.
async fn open(
    host: &str,
    port: u16,
    timeout: Duration,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    out: &mut Vec<u8>,
) -> io::Result<Result<TcpStream, DisconnectReason>> {
    let connecting = tokio::time::timeout(timeout, TcpStream::connect((host, port)));
    tokio::pin!(connecting);
    loop {
        tokio::select! {
            connected = &mut connecting => {
                return match connected {
                    Ok(stream) => Ok(Ok(stream?)),
                    Err(_) => Ok(Err(DisconnectReason::Failed("connect timed out".to_owned()))),
                };
            }
            command = commands.recv() => match command {
                Some(Command::Write(bytes)) => out.extend(bytes),
                Some(Command::Close) | None => return Ok(Err(DisconnectReason::ClosedByClient)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    fn options(host: &str, port: u16) -> ConnectOptions {
        ConnectOptions {
            host: host.to_owned(),
            port,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
        }
    }

    /// An address from TEST-NET-1 (RFC 5737): a connect to it never
    /// completes, or fails at once where the network has no route.
    const UNREACHABLE: &str = "192.0.2.1";

    async fn next(events: &mut mpsc::Receiver<Event>) -> Option<Event> {
        // A hang guard, long enough that a loaded machine never reaches it.
        tokio::time::timeout(Duration::from_secs(60), events.recv())
            .await
            .expect("the connection ends")
    }

    #[tokio::test]
    async fn bytes_cross_both_ways_and_the_close_is_reported() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (connection, mut events) = connect(
            &tokio::runtime::Handle::current(),
            options("127.0.0.1", port),
        );
        connection.write(b"queued while connecting".to_vec());
        let (mut server, _) = listener.accept().await.unwrap();
        assert_eq!(events.recv().await, Some(Event::Connected));

        let mut queued = [0; 23];
        server.read_exact(&mut queued).await.unwrap();
        assert_eq!(&queued, b"queued while connecting");

        server.write_all(b"\xFF raw").await.unwrap();
        assert_eq!(
            events.recv().await,
            Some(Event::Received(Bytes::from_static(b"\xFF raw")))
        );
        drop(server);
        let mut last = None;
        while let Some(event) = events.recv().await {
            last = Some(event);
        }
        assert_eq!(last, Some(Event::Closed(DisconnectReason::ClosedByServer)));
    }

    /// On the real clock: a paused one jumps to the next timer whenever the
    /// runtime is idle, which real sockets leave it while the OS catches up,
    /// so it can reach the connect timeout before the connect is done.
    #[tokio::test]
    async fn silence_after_a_read_is_reported_once() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (_connection, mut events) = connect(
            &tokio::runtime::Handle::current(),
            options("127.0.0.1", port),
        );
        let (mut server, _) = listener.accept().await.unwrap();
        assert_eq!(events.recv().await, Some(Event::Connected));

        server.write_all(b"Password: ").await.unwrap();
        assert!(matches!(events.recv().await, Some(Event::Received(_))));
        assert_eq!(events.recv().await, Some(Event::Quiet));
        let more = tokio::time::timeout(QUIET_AFTER * 3, events.recv()).await;
        assert!(more.is_err(), "quiet again without a read: {more:?}");
    }

    #[tokio::test]
    async fn a_refused_connection_reports_why() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let (_connection, mut events) = connect(
            &tokio::runtime::Handle::current(),
            options("127.0.0.1", port),
        );
        assert!(matches!(
            events.recv().await,
            Some(Event::Closed(DisconnectReason::Failed(_)))
        ));
    }

    #[tokio::test]
    async fn a_connect_that_hangs_times_out() {
        let (_connection, mut events) = connect(
            &tokio::runtime::Handle::current(),
            ConnectOptions {
                connect_timeout: Duration::from_millis(100),
                ..options(UNREACHABLE, 4000)
            },
        );
        assert!(matches!(
            next(&mut events).await,
            Some(Event::Closed(DisconnectReason::Failed(_)))
        ));
    }

    #[tokio::test]
    async fn closing_cancels_a_pending_connect() {
        let (connection, mut events) = connect(
            &tokio::runtime::Handle::current(),
            options(UNREACHABLE, 4000),
        );
        connection.close();
        assert_eq!(
            next(&mut events).await,
            Some(Event::Closed(DisconnectReason::ClosedByClient))
        );
    }

    #[tokio::test]
    async fn dropping_every_handle_cancels_a_pending_connect() {
        let (connection, mut events) = connect(
            &tokio::runtime::Handle::current(),
            options(UNREACHABLE, 4000),
        );
        drop(connection);
        assert_eq!(
            next(&mut events).await,
            Some(Event::Closed(DisconnectReason::ClosedByClient))
        );
    }
}
