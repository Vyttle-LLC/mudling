//! Telnet as MUDs speak it, without any I/O.
//!
//! [`Parser`] turns server bytes, in any chunking, into [`Event`]s and the
//! negotiation replies to send back. It slices server text between IAC bytes
//! with `memchr`, so text reaches the line pipeline as zero-copy [`Bytes`]
//! slices of the chunk it arrived in. A replay can drive it as well as a
//! socket; [`crate::net`] wraps it around TCP.
//!
//! The server may enable ECHO, EOR, GMCP, MSSP and MCCP2 (compression).
//! Every other option, NAWS included, is refused, and the client enables no
//! option of its own.

pub mod gmcp;
mod mccp;

pub use bytes::Bytes;

use memchr::memchr;
use std::fmt;

/// Telnet command bytes (RFC 854, RFC 885).
mod command {
    pub const IAC: u8 = 255;
    pub const DONT: u8 = 254;
    pub const DO: u8 = 253;
    pub const WONT: u8 = 252;
    pub const WILL: u8 = 251;
    pub const SB: u8 = 250;
    pub const GA: u8 = 249;
    pub const SE: u8 = 240;
    pub const EOR: u8 = 239;
}

/// Telnet options the parser knows by number.
pub mod option {
    pub const ECHO: u8 = 1;
    pub const EOR: u8 = 25;
    pub const NAWS: u8 = 31;
    pub const MSSP: u8 = 70;
    pub const COMPRESS2: u8 = 86;
    pub const GMCP: u8 = 201;
}

const MSSP_VAR: u8 = 1;
const MSSP_VAL: u8 = 2;

/// What happened on a connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// The TCP connection is open.
    Connected,
    /// Server text, a slice of the received (or decompressed) chunk.
    Data(Bytes),
    /// The server marked the end of a prompt with GA or EOR.
    PromptEnd,
    /// A GMCP message. `data` is the raw JSON text, empty when none was sent.
    Gmcp { package: String, data: String },
    /// The server agreed to GMCP, so the client can say hello and subscribe.
    GmcpEnabled,
    /// Whether the client should echo typed input. The server turns echo off
    /// (by offering to echo itself) while a password is typed.
    Echo(bool),
    /// The server's name, from MSSP.
    Mssp(String),
    /// The connection closed.
    Disconnected(DisconnectReason),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DisconnectReason {
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

/// The compressed stream is corrupt. Nothing after it can be read, so the
/// connection should close.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompressionError(pub String);

impl fmt::Display for CompressionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MCCP2 decompression failed: {}", self.0)
    }
}

impl std::error::Error for CompressionError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Text,
    Iac,
    Negotiation { verb: u8 },
    SubnegotiationOption,
    Subnegotiation { option: u8 },
    SubnegotiationIac { option: u8 },
}

/// The telnet state of one connection.
#[derive(Debug)]
pub struct Parser {
    state: State,
    /// The payload of the subnegotiation being read, which may span chunks.
    subnegotiation: Vec<u8>,
    /// Options the server has enabled, by option number.
    server_enabled: [bool; 256],
    inflater: Option<mccp::Inflater>,
    output: Vec<u8>,
}

impl Default for Parser {
    fn default() -> Self {
        Parser {
            state: State::Text,
            subnegotiation: Vec::new(),
            server_enabled: [false; 256],
            inflater: None,
            output: Vec::new(),
        }
    }
}

impl Parser {
    pub fn new() -> Self {
        Parser::default()
    }

    /// Parses one chunk of server bytes, appending what happened to `events`.
    /// Replies to negotiation collect in [`Parser::take_output`].
    pub fn feed(&mut self, chunk: Bytes, events: &mut Vec<Event>) -> Result<(), CompressionError> {
        let mut rest = chunk;
        while !rest.is_empty() {
            if self.inflater.is_some() {
                rest = self.inflate(rest, events)?;
            } else {
                match self.parse(&rest, false, events) {
                    Some(compressed_from) => {
                        self.inflater = Some(mccp::Inflater::new());
                        rest = rest.slice(compressed_from..);
                    }
                    None => break,
                }
            }
        }
        Ok(())
    }

    /// Bytes to send to the server: replies to its negotiation.
    pub fn take_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.output)
    }

    /// Decompresses `input` and parses the result, returning whatever follows
    /// the end of the compressed stream.
    fn inflate(
        &mut self,
        mut input: Bytes,
        events: &mut Vec<Event>,
    ) -> Result<Bytes, CompressionError> {
        loop {
            let inflater = self.inflater.as_mut().expect("compression is on");
            let inflated = match inflater.inflate(&input) {
                Ok(inflated) => inflated,
                Err(error) => {
                    self.inflater = None;
                    return Err(error);
                }
            };
            input = input.slice(inflated.consumed..);
            if !inflated.output.is_empty() {
                self.parse(&inflated.output, true, events);
            }
            if inflated.ended {
                self.inflater = None;
                return Ok(input);
            }
            if input.is_empty() && !inflated.more {
                return Ok(input);
            }
        }
    }

    /// Parses uncompressed telnet. Returns the offset where compression
    /// starts, if the server starts it here; `compressed` data can't restart it.
    fn parse(&mut self, data: &Bytes, compressed: bool, events: &mut Vec<Event>) -> Option<usize> {
        let mut at = 0;
        while at < data.len() {
            match self.state {
                State::Text => at = self.text_run(data, at, at, events),
                State::Iac => {
                    let byte = data[at];
                    at += 1;
                    self.state = State::Text;
                    match byte {
                        // An escaped 0xFF is text: it starts the next run.
                        command::IAC => at = self.text_run(data, at - 1, at, events),
                        command::WILL | command::WONT | command::DO | command::DONT => {
                            self.state = State::Negotiation { verb: byte };
                        }
                        command::SB => self.state = State::SubnegotiationOption,
                        command::GA | command::EOR => events.push(Event::PromptEnd),
                        _ => {}
                    }
                }
                State::Negotiation { verb } => {
                    self.state = State::Text;
                    self.negotiate(verb, data[at], events);
                    at += 1;
                }
                State::SubnegotiationOption => {
                    self.subnegotiation.clear();
                    self.state = State::Subnegotiation { option: data[at] };
                    at += 1;
                }
                State::Subnegotiation { option } => {
                    let end = memchr(command::IAC, &data[at..]).map_or(data.len(), |i| at + i);
                    self.subnegotiation.extend_from_slice(&data[at..end]);
                    if end < data.len() {
                        self.state = State::SubnegotiationIac { option };
                        at = end + 1;
                    } else {
                        at = end;
                    }
                }
                State::SubnegotiationIac { option } => {
                    let byte = data[at];
                    at += 1;
                    self.state = State::Subnegotiation { option };
                    match byte {
                        command::SE => {
                            self.state = State::Text;
                            if self.finish_subnegotiation(option, events) && !compressed {
                                return Some(at);
                            }
                        }
                        command::IAC => self.subnegotiation.push(command::IAC),
                        // Malformed: drop the byte and keep reading the payload.
                        _ => {}
                    }
                }
            }
        }
        None
    }

    /// Emits text from `start` up to the next IAC found from `search_from`,
    /// and returns the offset after that IAC (or the end of `data`).
    fn text_run(
        &mut self,
        data: &Bytes,
        start: usize,
        search_from: usize,
        events: &mut Vec<Event>,
    ) -> usize {
        let iac = memchr(command::IAC, &data[search_from..]).map(|i| search_from + i);
        let end = iac.unwrap_or(data.len());
        if end > start {
            events.push(Event::Data(data.slice(start..end)));
        }
        match iac {
            Some(iac) => {
                self.state = State::Iac;
                iac + 1
            }
            None => end,
        }
    }

    fn negotiate(&mut self, verb: u8, option: u8, events: &mut Vec<Event>) {
        let enabled = self.server_enabled[usize::from(option)];
        match verb {
            command::WILL if server_may_enable(option) => {
                if !enabled {
                    self.server_enabled[usize::from(option)] = true;
                    self.reply(command::DO, option);
                    self.server_changed(option, true, events);
                }
            }
            command::WILL => self.reply(command::DONT, option),
            command::WONT => {
                if enabled {
                    self.server_enabled[usize::from(option)] = false;
                    self.reply(command::DONT, option);
                    self.server_changed(option, false, events);
                }
            }
            // The client enables nothing itself, so it refuses every DO and
            // a DONT asks for what is already the case.
            command::DO => self.reply(command::WONT, option),
            _ => {}
        }
    }

    fn reply(&mut self, verb: u8, option: u8) {
        self.output.extend_from_slice(&[command::IAC, verb, option]);
    }

    fn server_changed(&mut self, option: u8, enabled: bool, events: &mut Vec<Event>) {
        match option {
            option::ECHO => events.push(Event::Echo(!enabled)),
            option::GMCP if enabled => events.push(Event::GmcpEnabled),
            _ => {}
        }
    }

    /// Handles a complete subnegotiation. Returns whether compression starts.
    fn finish_subnegotiation(&mut self, option: u8, events: &mut Vec<Event>) -> bool {
        match option {
            option::GMCP => events.extend(gmcp_event(&self.subnegotiation)),
            option::MSSP => events.extend(mssp_name(&self.subnegotiation).map(Event::Mssp)),
            option::COMPRESS2 => return self.server_enabled[usize::from(option::COMPRESS2)],
            _ => {}
        }
        false
    }
}

fn server_may_enable(option: u8) -> bool {
    matches!(
        option,
        option::ECHO | option::EOR | option::MSSP | option::COMPRESS2 | option::GMCP
    )
}

/// `Package.Name {json}`, as the wire carries it.
fn gmcp_event(payload: &[u8]) -> Option<Event> {
    let text = String::from_utf8_lossy(payload);
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let (package, data) = text
        .split_once(char::is_whitespace)
        .map_or((text, ""), |(package, data)| (package, data.trim_start()));
    Some(Event::Gmcp {
        package: package.to_owned(),
        data: data.to_owned(),
    })
}

/// The first `NAME` value in an MSSP payload (`VAR name VAL value …`).
fn mssp_name(payload: &[u8]) -> Option<String> {
    for variable in payload.split(|&byte| byte == MSSP_VAR).skip(1) {
        let mut parts = variable.split(|&byte| byte == MSSP_VAL);
        if parts.next() == Some(b"NAME") {
            return parts
                .next()
                .map(|name| String::from_utf8_lossy(name).into_owned());
        }
    }
    None
}

/// Appends a command line to `out`: 0xFF bytes doubled, then CRLF. The line
/// is bytes because a game's encoding (Latin-1) can hold 0xFF.
pub fn encode_line(line: &[u8], out: &mut Vec<u8>) {
    escape_into(line, out);
    out.extend_from_slice(b"\r\n");
}

/// Appends `bytes` to `out` with every 0xFF doubled, as telnet requires of
/// data and subnegotiation payloads.
fn escape_into(mut bytes: &[u8], out: &mut Vec<u8>) {
    while let Some(iac) = memchr(command::IAC, bytes) {
        out.extend_from_slice(&bytes[..=iac]);
        out.push(command::IAC);
        bytes = &bytes[iac + 1..];
    }
    out.extend_from_slice(bytes);
}

#[cfg(test)]
mod tests;
