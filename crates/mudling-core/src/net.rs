//! The connection to a server: TCP through tokio, around [`telnet::Parser`].
//!
//! [`connect`] spawns the connection on a core runtime and returns at once,
//! so any thread can call it. The [`Connection`] handle sends without
//! blocking; events arrive on a channel, starting with [`Event::Connected`]
//! and ending with [`Event::Disconnected`].

use crate::telnet::{self, DisconnectReason, Event, gmcp};
use bytes::BytesMut;
use std::io;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

const READ_SIZE: usize = 64 * 1024;
const EVENT_QUEUE: usize = 1024;

/// Answers login prompts. The prompts and answers come from a plugin; the
/// connection only shows it each event until it says login is over.
pub trait LoginHook: Send {
    /// The line to send in answer to `event`, if it is a prompt.
    fn answer(&mut self, event: &Event) -> Option<String>;
    /// Whether login is over, so the hook sees no more events.
    fn is_done(&self) -> bool;
}

pub struct ConnectOptions {
    pub host: String,
    pub port: u16,
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
    let stream = TcpStream::connect((options.host.as_str(), options.port)).await?;
    stream.set_nodelay(true)?;
    if events.send(Event::Connected).await.is_err() {
        return Ok(DisconnectReason::ClosedByClient);
    }
    let (mut reader, mut writer) = stream.into_split();
    let mut parser = telnet::Parser::new();
    let mut parsed = Vec::new();
    let mut buffer = BytesMut::new();
    let mut out = Vec::new();
    loop {
        buffer.reserve(READ_SIZE);
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
                match command {
                    Some(Command::Line(line)) => telnet::encode_line(&line, &mut out),
                    Some(Command::Gmcp { package, data }) => gmcp::encode(&package, &data, &mut out),
                    Some(Command::Close) | None => {
                        let _ = writer.shutdown().await;
                        return Ok(DisconnectReason::ClosedByClient);
                    }
                }
                writer.write_all(&out).await?;
                out.clear();
            }
        }
    }
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
            telnet::encode_line(line.as_bytes(), out);
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
        fn answer(&mut self, event: &Event) -> Option<String> {
            match event {
                Event::Data(text) if text.ends_with(b"name? ") => {
                    self.answered = true;
                    Some("Tester".to_owned())
                }
                _ => None,
            }
        }

        fn is_done(&self) -> bool {
            self.answered
        }
    }

    #[tokio::test]
    async fn negotiates_says_hello_answers_login_and_sends_lines() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (connection, mut events) = connect(
            &tokio::runtime::Handle::current(),
            ConnectOptions {
                host: "127.0.0.1".to_owned(),
                port,
                client_name: "MUDling".to_owned(),
                client_version: "0.0.0".to_owned(),
                gmcp_supports: vec!["Char 1".to_owned()],
                login: Some(Box::new(NameOnce::default())),
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
            ConnectOptions {
                host: "127.0.0.1".to_owned(),
                port,
                client_name: "MUDling".to_owned(),
                client_version: "0.0.0".to_owned(),
                gmcp_supports: Vec::new(),
                login: None,
            },
        );
        assert!(matches!(
            events.recv().await,
            Some(Event::Disconnected(DisconnectReason::Failed(_)))
        ));
    }
}
