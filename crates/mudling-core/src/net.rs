//! The connection to a server: TCP through tokio, around [`telnet::Parser`].
//!
//! [`connect`] spawns the connection on a core runtime and returns at once,
//! so any thread can call it. The [`Connection`] handle sends without
//! blocking; events arrive on a channel, starting with [`Event::Connected`]
//! and ending with [`Event::Disconnected`].

use crate::telnet::{self, DisconnectReason, Event, gmcp};
use bytes::BytesMut;
use std::io;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// A fresh read buffer's size, allocated when the current one has less than
/// `MIN_READ` left. It is reclaimed in place once the text slices cut from it
/// have been dropped.
const READ_SIZE: usize = 64 * 1024;
const MIN_READ: usize = 4 * 1024;
const EVENT_QUEUE: usize = 1024;

/// Answers login prompts. The prompts and answers come from a plugin; the
/// connection only shows it each event until it says login is over.
pub trait LoginHook: Send {
    /// The line to send in answer to `event`, if it is a prompt, already in
    /// the server's encoding.
    fn answer(&mut self, event: &Event) -> Option<Vec<u8>>;
    /// Whether login is over, so the hook sees no more events.
    fn is_done(&self) -> bool;
}

pub struct ConnectOptions {
    pub host: String,
    pub port: u16,
    /// How long to wait for the TCP connection, usually
    /// [`DEFAULT_CONNECT_TIMEOUT`].
    pub connect_timeout: Duration,
    pub client_name: String,
    pub client_version: String,
    /// GMCP modules to subscribe to once the server agrees to GMCP, such as
    /// `"Char 1"`.
    pub gmcp_supports: Vec<String>,
    pub login: Option<Box<dyn LoginHook>>,
}

/// Sends to the server. Clones share the connection; it closes when
/// [`Connection::close`] is called or every clone is dropped. Sends after the
/// connection closed are dropped.
#[derive(Clone, Debug)]
pub struct Connection {
    commands: mpsc::UnboundedSender<Command>,
}

#[derive(Debug)]
enum Command {
    Line(Vec<u8>),
    Gmcp { package: String, data: String },
    Close,
}

impl Connection {
    /// Sends a command line, already in the server's encoding.
    pub fn send_line(&self, line: impl Into<Vec<u8>>) {
        let _ = self.commands.send(Command::Line(line.into()));
    }

    pub fn send_gmcp(&self, package: impl Into<String>, data: impl Into<String>) {
        let _ = self.commands.send(Command::Gmcp {
            package: package.into(),
            data: data.into(),
        });
    }

    pub fn close(&self) {
        let _ = self.commands.send(Command::Close);
    }
}

/// Connects on `runtime`, which must have I/O enabled.
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
        let _ = event_queue.send(Event::Disconnected(reason)).await;
    });
    (Connection { commands }, events)
}

async fn run(
    mut options: ConnectOptions,
    mut commands: mpsc::UnboundedReceiver<Command>,
    events: &mpsc::Sender<Event>,
) -> io::Result<DisconnectReason> {
    let mut out = Vec::new();
    let address = (options.host.as_str(), options.port, options.connect_timeout);
    let stream = match open(address, &mut commands, &mut out).await? {
        Ok(stream) => stream,
        Err(reason) => return Ok(reason),
    };
    stream.set_nodelay(true)?;
    if events.send(Event::Connected).await.is_err() {
        return Ok(DisconnectReason::ClosedByClient);
    }
    let (mut reader, mut writer) = stream.into_split();
    writer.write_all(&out).await?;
    out.clear();
    let mut parser = telnet::Parser::new();
    let mut parsed = Vec::new();
    let mut buffer = BytesMut::new();
    loop {
        if buffer.capacity() < MIN_READ {
            buffer.reserve(READ_SIZE);
        }
        tokio::select! {
            read = reader.read_buf(&mut buffer) => {
                if read? == 0 {
                    return Ok(DisconnectReason::ClosedByServer);
                }
                let fed = parser.feed(buffer.split().freeze(), &mut parsed);
                out.extend(parser.take_output());
                for event in &parsed {
                    answer(event, &mut options, &mut out);
                }
                writer.write_all(&out).await?;
                out.clear();
                for event in parsed.drain(..) {
                    if events.send(event).await.is_err() {
                        return Ok(DisconnectReason::ClosedByClient);
                    }
                }
                if let Err(error) = fed {
                    return Ok(DisconnectReason::Failed(error.to_string()));
                }
            }
            command = commands.recv() => {
                if !encode_command(command, &mut out) {
                    let _ = writer.shutdown().await;
                    return Ok(DisconnectReason::ClosedByClient);
                }
                writer.write_all(&out).await?;
                out.clear();
            }
        }
    }
}

/// Opens the TCP connection within the timeout. Lines sent meanwhile wait in
/// `out`; a close, or every handle dropped, aborts the connect.
async fn open(
    (host, port, timeout): (&str, u16, Duration),
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
            command = commands.recv() => {
                if !encode_command(command, out) {
                    return Ok(Err(DisconnectReason::Failed("closed while connecting".to_owned())));
                }
            }
        }
    }
}

/// Appends a command's bytes to `out`. Returns false when the command closes
/// the connection: a close, or every handle dropped.
fn encode_command(command: Option<Command>, out: &mut Vec<u8>) -> bool {
    match command {
        Some(Command::Line(line)) => telnet::encode_line(&line, out),
        Some(Command::Gmcp { package, data }) => gmcp::encode(&package, &data, out),
        Some(Command::Close) | None => return false,
    }
    true
}

/// Writes what the client says in answer to `event`: its GMCP hello and
/// subscriptions, and any login answer.
fn answer(event: &Event, options: &mut ConnectOptions, out: &mut Vec<u8>) {
    if *event == Event::GmcpEnabled {
        gmcp::encode_hello(&options.client_name, &options.client_version, out);
        gmcp::encode_supports_set(&options.gmcp_supports, out);
    }
    if let Some(login) = &mut options.login {
        if let Some(line) = login.answer(event) {
            telnet::encode_line(&line, out);
        }
        if login.is_done() {
            options.login = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    const IAC: u8 = 255;
    const SB: u8 = 250;
    const SE: u8 = 240;
    const WILL: u8 = 251;
    const DO: u8 = 253;

    /// Answers the name prompt, then is done.
    #[derive(Default)]
    struct NameOnce {
        answered: bool,
    }

    impl LoginHook for NameOnce {
        fn answer(&mut self, event: &Event) -> Option<Vec<u8>> {
            match event {
                Event::Data(text) if text.ends_with(b"name? ") => {
                    self.answered = true;
                    Some(b"Tester".to_vec())
                }
                _ => None,
            }
        }

        fn is_done(&self) -> bool {
            self.answered
        }
    }

    fn options(host: &str, port: u16) -> ConnectOptions {
        ConnectOptions {
            host: host.to_owned(),
            port,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            client_name: "MUDling".to_owned(),
            client_version: "0.0.0".to_owned(),
            gmcp_supports: vec!["Char 1".to_owned()],
            login: None,
        }
    }

    /// An address from TEST-NET-1 (RFC 5737): a connect to it never
    /// completes, or fails at once where the network has no route.
    const UNREACHABLE: &str = "192.0.2.1";

    async fn failure(events: &mut mpsc::Receiver<Event>) -> Option<Event> {
        tokio::time::timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("the connection ends promptly")
    }

    #[tokio::test]
    async fn negotiates_says_hello_answers_login_and_sends_lines() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (connection, mut events) = connect(
            &tokio::runtime::Handle::current(),
            ConnectOptions {
                login: Some(Box::new(NameOnce::default())),
                ..options("127.0.0.1", port)
            },
        );
        let (mut server, _) = listener.accept().await.unwrap();
        assert_eq!(events.recv().await, Some(Event::Connected));

        server.write_all(&[IAC, WILL, 201]).await.unwrap();
        server.write_all(b"What is your name? ").await.unwrap();
        let mut expected = vec![IAC, DO, 201];
        gmcp::encode_hello("MUDling", "0.0.0", &mut expected);
        gmcp::encode_supports_set(&["Char 1"], &mut expected);
        expected.extend_from_slice(b"Tester\r\n");
        let mut received = vec![0; expected.len()];
        server.read_exact(&mut received).await.unwrap();
        assert_eq!(received, expected);

        connection.send_line(vec![b'h', 0xFF, b'i']);
        let mut line = [0; 6];
        server.read_exact(&mut line).await.unwrap();
        assert_eq!(line, *b"h\xFF\xFFi\r\n", "IAC is doubled");

        server.write_all(&[IAC, SB, 201]).await.unwrap();
        server.write_all(b"Char.Vitals {\"hp\":1}").await.unwrap();
        server.write_all(&[IAC, SE]).await.unwrap();
        drop(server);

        let mut seen = Vec::new();
        while let Some(event) = events.recv().await {
            seen.push(event);
        }
        assert!(seen.contains(&Event::GmcpEnabled));
        assert!(seen.contains(&Event::Gmcp {
            package: "Char.Vitals".to_owned(),
            data: "{\"hp\":1}".to_owned()
        }));
        assert_eq!(
            seen.last(),
            Some(&Event::Disconnected(DisconnectReason::ClosedByServer))
        );
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
            Some(Event::Disconnected(DisconnectReason::Failed(_)))
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
            failure(&mut events).await,
            Some(Event::Disconnected(DisconnectReason::Failed(_)))
        ));
    }

    #[tokio::test]
    async fn closing_aborts_a_pending_connect() {
        let (connection, mut events) = connect(
            &tokio::runtime::Handle::current(),
            options(UNREACHABLE, 4000),
        );
        connection.close();
        assert!(matches!(
            failure(&mut events).await,
            Some(Event::Disconnected(DisconnectReason::Failed(_)))
        ));
    }

    #[tokio::test]
    async fn dropping_every_handle_aborts_a_pending_connect() {
        let (connection, mut events) = connect(
            &tokio::runtime::Handle::current(),
            options(UNREACHABLE, 4000),
        );
        drop(connection);
        assert!(matches!(
            failure(&mut events).await,
            Some(Event::Disconnected(DisconnectReason::Failed(_)))
        ));
    }
}
