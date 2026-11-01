//! What a session records, as it happens. Nothing here touches a log's file
//! except reading the settings, when a connection starts or the player
//! changes them.
//!
//! Input never reaches the debug log: only what the server sent comes here
//! for it. While the server has echo off, nothing is recorded in either log,
//! as a server may echo what's typed back. A line the chat would finish after
//! echo comes back on can hold such text, so it's left out too.

use super::writer::{LogWriter, Trouble};
use super::{Kind, LogSettings, Logs};
use crate::line::{Line, LineKind};
use crate::replay::{CONNECTED_NOTE, DISCONNECTED_NOTE, GMCP_NOTE, SENT_NOTE};
use memchr::memchr2;
use std::path::Path;
use std::thread::JoinHandle;

/// One session's logs, one pair at a time: a connection has its own.
pub(crate) struct SessionLogs {
    logs: Logs,
    host: String,
    connected: bool,
    debug: Option<LogWriter>,
    chat: Option<LogWriter>,
    echo_off: bool,
    /// The line being read began or grew with echo off.
    line_withheld: bool,
    closing: Vec<JoinHandle<()>>,
    notices: Vec<String>,
}

impl SessionLogs {
    pub(crate) fn new(data_folder: &Path, host: &str) -> SessionLogs {
        SessionLogs {
            logs: Logs::in_data_folder(data_folder),
            host: host.to_owned(),
            connected: false,
            debug: None,
            chat: None,
            echo_off: false,
            line_withheld: false,
            closing: Vec::new(),
            notices: Vec::new(),
        }
    }

    /// A connection starts: it logs as the player has chosen.
    pub(crate) fn connection_started(&mut self) {
        self.connection_ended(None);
        self.connected = true;
        self.follow_settings();
    }

    /// Starts and ends logs to match what the player has chosen. A log that
    /// goes off ends now; one that comes on starts with the next lines.
    pub(crate) fn follow_settings(&mut self) {
        if !self.connected {
            return;
        }
        let LogSettings { debug, chat } = self.logs.settings();
        let chat = chat.contains(&self.host);
        self.follow(Kind::Debug, debug);
        self.follow(Kind::Chat, chat);
    }

    fn follow(&mut self, kind: Kind, on: bool) {
        let open = self.writer(kind).is_some();
        if on && !open {
            match LogWriter::start(&self.logs, kind, &self.host) {
                Ok(writer) => {
                    *self.writer(kind) = Some(writer);
                    self.push(kind, &[CONNECTED_NOTE.as_bytes(), b"\n"]);
                }
                Err(error) => Self::tell(
                    &mut self.notices,
                    kind,
                    Some(Trouble::Failed(error.to_string())),
                ),
            }
        } else if !on && let Some(writer) = self.writer(kind).take() {
            self.closing.extend(writer.close());
        }
    }

    fn writer(&mut self, kind: Kind) -> &mut Option<LogWriter> {
        match kind {
            Kind::Debug => &mut self.debug,
            Kind::Chat => &mut self.chat,
        }
    }

    /// The connection ended, for `reason` if it was one. Files finish behind
    /// it.
    pub(crate) fn connection_ended(&mut self, reason: Option<&str>) {
        if let Some(reason) = reason {
            let said = format!("\n{DISCONNECTED_NOTE}{reason}\n");
            self.push(Kind::Debug, &[said.as_bytes()]);
        }
        self.closing.extend(
            [self.debug.take(), self.chat.take()]
                .into_iter()
                .flatten()
                .filter_map(LogWriter::close),
        );
        self.closing.retain(|thread| !thread.is_finished());
        self.connected = false;
        self.echo_off = false;
        self.line_withheld = false;
    }

    /// The server turned its echo off or on. `line_open` says it left a line
    /// unfinished, which what's typed with echo off could have joined.
    pub(crate) fn echo_changed(&mut self, off: bool, line_open: bool) {
        if self.echo_off && !off {
            self.line_withheld = line_open;
        }
        self.echo_off = off;
    }

    /// Text the server sent, as it sent it.
    pub(crate) fn server_text(&mut self, text: &[u8]) {
        if !self.echo_off {
            self.push(Kind::Debug, &[text]);
        }
    }

    /// A GMCP message. A note is one line, and JSON may span several.
    pub(crate) fn gmcp(&mut self, package: &str, json: &str) {
        if self.echo_off || self.debug.is_none() {
            return;
        }
        let one_line;
        let json = if memchr2(b'\n', b'\r', json.as_bytes()).is_some() {
            one_line = json.replace(['\n', '\r'], " ");
            &one_line
        } else {
            json
        };
        self.push(
            Kind::Debug,
            &[
                b"\n",
                GMCP_NOTE.as_bytes(),
                package.as_bytes(),
                b" ",
                json.as_bytes(),
                b"\n",
            ],
        );
    }

    /// A line the player saw. A command shows as a note, so a replay sends
    /// it to no server.
    pub(crate) fn shown(&mut self, line: &Line) {
        if self.chat.is_none() || self.echo_off || line.is_only_prompt() {
            return;
        }
        let text = line.shown_text(false).as_bytes();
        match line.kind() {
            LineKind::Command => {
                self.push(Kind::Chat, &[b"\n", SENT_NOTE.as_bytes(), text, b"\n"]);
            }
            _ if std::mem::take(&mut self.line_withheld) => {}
            _ => self.push(Kind::Chat, &[text, b"\n"]),
        }
    }

    fn push(&mut self, kind: Kind, parts: &[&[u8]]) {
        if let Some(writer) = self.writer(kind) {
            let trouble = writer.push(parts);
            Self::tell(&mut self.notices, kind, trouble);
        }
    }

    fn tell(notices: &mut Vec<String>, kind: Kind, trouble: Option<Trouble>) {
        let name = kind.name();
        notices.extend(trouble.map(|trouble| match trouble {
            Trouble::FellBehind => {
                format!("the {name} log lost lines: the disk fell behind the server")
            }
            Trouble::Full => format!("the {name} log reached its size limit and stopped"),
            Trouble::Failed(error) => format!("the {name} log stopped: {error}"),
        }));
    }

    /// What the player should hear about the logs, since the last call.
    pub(crate) fn take_notices(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notices)
    }

    /// Waits for every log to reach the disk.
    pub(crate) fn finish(mut self) {
        self.connection_ended(None);
        for thread in self.closing {
            let _ = thread.join();
        }
    }
}
