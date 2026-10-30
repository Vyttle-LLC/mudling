//! The window's root view: a centered column with the chat above the status
//! lines and the command input, a thin shell over a [`Session`].
//!
//! The UI thread only subscribes, applies and draws. The session's waker
//! comes from a core thread; each wake asks for the next frame, and that
//! frame drains the stores once, so updates apply at most once per frame.

use crate::chat::{COLUMN_WIDTH, Chat, MONO_FONT};
use crate::input::{CommandInput, Submitted};
use crate::status::{StatusLines, lines_for};
use crate::theme::Theme;
use futures::StreamExt;
use futures::channel::mpsc::UnboundedReceiver;
use gpui::{Context, Entity, Focusable, Subscription, Task, Window, div, prelude::*};
use mudling_core::plugin::PluginId;
use mudling_core::session::Session;
use mudling_core::store::{ConnectionStatus, SettingsState, Versioned, VitalsState};
use mudling_core::telnet::Bytes;
use tempfile::TempDir;

/// What the window runs on. Without one the window only echoes what's typed.
pub struct Game {
    pub session: Session,
    /// A recording's throwaway data folder, or `None` for a real server.
    pub replay_folder: Option<TempDir>,
    /// The game plugins the player turned on.
    pub enabled: Vec<PluginId>,
}

/// The shell's copies of the stores it shows, other than the terminal.
struct Copies {
    settings: Versioned<SettingsState>,
    vitals: Versioned<VitalsState>,
}

pub struct Shell {
    game: Option<Game>,
    copies: Copies,
    left: bool,
    chat: Entity<Chat>,
    status: Entity<StatusLines>,
    input: Entity<CommandInput>,
    _subscriptions: [Subscription; 3],
    _woken: Task<()>,
}

impl Shell {
    /// `woken` hears from the session's waker. A real server's connection
    /// opens here, once the stores are subscribed.
    pub fn new(
        game: Option<Game>,
        woken: UnboundedReceiver<()>,
        window: &mut Window,
        cx: &mut Context<Shell>,
    ) -> Shell {
        cx.set_global(Theme::for_appearance(window.appearance()));
        let stores = game.as_ref().map(|game| game.session.stores());
        let terminal = stores.map(|stores| stores.subscribe()).unwrap_or_default();
        let copies = Copies {
            settings: stores.map(|stores| stores.subscribe()).unwrap_or_default(),
            vitals: stores.map(|stores| stores.subscribe()).unwrap_or_default(),
        };
        let chat = cx.new(|_| Chat::new(terminal));
        let input = cx.new(CommandInput::new);
        let submitted = cx.subscribe(&input, |shell, _, Submitted(command), cx| {
            shell.submit(command, cx);
        });
        let appearance = cx.observe_window_appearance(window, |_, window, cx| {
            cx.set_global(Theme::for_appearance(window.appearance()));
            cx.notify();
        });
        let quitting = cx.on_app_quit(|shell, cx| {
            let saved = shell.leave(cx);
            async move {
                if let Some(saved) = saved {
                    saved.await;
                }
            }
        });
        window.focus(&input.focus_handle(cx), cx);
        if let Some(game) = game.as_ref().filter(|game| game.replay_folder.is_none()) {
            game.session.connect();
        }
        let mut shell = Shell {
            game,
            copies,
            left: false,
            chat,
            status: cx.new(|_| StatusLines::default()),
            input,
            _subscriptions: [submitted, appearance, quitting],
            _woken: Self::apply_stores_when_woken(woken, window, cx),
        };
        shell.show_status(cx);
        shell
    }

    /// Each wake is one batch waiting, and no other wake comes until a frame
    /// drains it.
    fn apply_stores_when_woken(
        mut woken: UnboundedReceiver<()>,
        window: &mut Window,
        cx: &mut Context<Shell>,
    ) -> Task<()> {
        cx.spawn_in(window, async move |shell, cx| {
            while woken.next().await.is_some() {
                let asked = shell.update_in(cx, |_, window, cx| {
                    cx.on_next_frame(window, |shell, window, cx| shell.apply_stores(window, cx));
                });
                if asked.is_err() {
                    return;
                }
            }
        })
    }

    /// Applies what changed since the last frame.
    fn apply_stores(&mut self, window: &mut Window, cx: &mut Context<Shell>) {
        let stores = self.game.as_ref().map(|game| game.session.stores());
        let Some(batch) = stores.and_then(|stores| stores.drain()) else {
            return;
        };
        if let Some(lines) = &batch.terminal {
            self.chat.update(cx, |chat, cx| chat.apply(lines, cx));
        }
        if let Some(vitals) = &batch.vitals {
            self.copies.vitals.apply_diff(vitals);
        }
        if let Some(settings) = &batch.settings {
            self.copies.settings.apply_diff(settings);
            let masked = self.copies.settings.value().echo_off;
            self.input
                .update(cx, |input, cx| input.set_masked(masked, window, cx));
        }
        if batch.vitals.is_some() || batch.settings.is_some() {
            self.show_status(cx);
        }
    }

    fn show_status(&mut self, cx: &mut Context<Shell>) {
        let Some(game) = &self.game else { return };
        let lines = lines_for(
            self.copies.settings.value(),
            self.copies.vitals.value(),
            &game.enabled,
            game.replay_folder.is_some(),
        );
        self.status.update(cx, |status, cx| status.show(lines, cx));
    }

    /// Sends what was typed; the session echoes it, unless it's a password.
    /// With no server connected it goes nowhere, so the chat shows it here.
    fn submit(&mut self, command: &str, cx: &mut Context<Shell>) {
        let settings = self.copies.settings.value();
        let connected = settings.connection == ConnectionStatus::Connected;
        match &self.game {
            Some(game) if connected && game.replay_folder.is_none() => game.session.send(command),
            _ if settings.echo_off => {}
            _ => self
                .chat
                .update(cx, |chat, cx| chat.echo_unsent(command, cx)),
        }
    }

    /// Feeds a recording through the session, as one connection.
    pub fn replay(&self, reads: Vec<Bytes>) {
        if let Some(game) = &self.game {
            game.session.replay(reads);
        }
    }

    /// Whether the first recording has reached the chat.
    pub fn replay_finished(&self) -> bool {
        matches!(
            self.copies.settings.value().connection,
            ConnectionStatus::Closed(_)
        )
    }

    pub fn chat(&self) -> Entity<Chat> {
        self.chat.clone()
    }

    /// Ends the session, off this thread. The task finishes once everything
    /// reached the disk. `None` if the shell already left.
    pub fn leave(&mut self, cx: &mut Context<Shell>) -> Option<Task<()>> {
        if std::mem::replace(&mut self.left, true) {
            return None;
        }
        let game = self.game.take();
        Some(cx.background_spawn(async move {
            let Some(game) = game else { return };
            if let Err(error) = game.session.quit().wait() {
                eprintln!("{error}");
            }
            drop(game.replay_folder);
        }))
    }
}

impl Render for Shell {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Shell>) -> impl IntoElement {
        let theme = cx.global::<Theme>();
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .bg(theme.background)
            .text_color(theme.text)
            .font_family(MONO_FONT)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .max_w(COLUMN_WIDTH)
                    .px_4()
                    .pt_4()
                    .child(self.chat.clone()),
            )
            .child(
                div()
                    .w_full()
                    .max_w(COLUMN_WIDTH)
                    .px_4()
                    .pt_2()
                    .child(self.status.clone()),
            )
            .child(
                div()
                    .w_full()
                    .max_w(COLUMN_WIDTH)
                    .p_4()
                    .child(self.input.clone()),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::channel::mpsc::{UnboundedSender, unbounded};
    use gpui::{TestAppContext, VisualTestContext, px};
    use mudling_core::session::SessionConfig;
    use mudling_core::store::TerminalState;
    use mudling_core::telnet::command::{IAC, SB, SE, WILL, WONT};
    use mudling_core::telnet::option::{ECHO, GMCP};
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::path::Path;
    use std::thread::sleep;
    use std::time::{Duration, Instant};

    const WAIT: Duration = Duration::from_secs(5);

    /// gpui's test scheduler fails on a wake from another thread, so the
    /// tests wake the shell themselves.
    fn open_game(folder: &Path, port: u16) -> Game {
        let config = SessionConfig::new(folder, "127.0.0.1", port);
        let session = Session::open(config, || {}).expect("the session starts");
        Game {
            session,
            replay_folder: None,
            enabled: Vec::new(),
        }
    }

    /// A window on a session, with its folder and a real server on loopback.
    struct Played<'a> {
        shell: Entity<Shell>,
        cx: &'a mut VisualTestContext,
        server: TcpStream,
        wake: UnboundedSender<()>,
        _folder: TempDir,
    }

    fn play(cx: &mut TestAppContext) -> Played<'_> {
        cx.update(crate::input::bind_keys);
        let folder = TempDir::new().unwrap();
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let (wake, woken) = unbounded();
        let game = open_game(folder.path(), port);
        let (shell, cx) =
            cx.add_window_view(|window, cx| Shell::new(Some(game), woken, window, cx));
        let (server, _) = listener.accept().unwrap();
        server.set_read_timeout(Some(WAIT)).unwrap();
        Played {
            shell,
            cx,
            server,
            wake,
            _folder: folder,
        }
    }

    impl Played<'_> {
        /// Runs frames until `done`, as the display would.
        fn frames_until(&mut self, what: &str, done: impl Fn(&Shell, &gpui::App) -> bool) {
            let started = Instant::now();
            while !self.shell.read_with(self.cx, |shell, cx| done(shell, cx)) {
                assert!(started.elapsed() < WAIT, "timed out waiting for {what}");
                self.frame();
                sleep(Duration::from_millis(1));
            }
        }

        /// What a display frame does. Test windows run no frames of their
        /// own, so what a wake asked for waits here.
        fn frame(&mut self) {
            self.cx.run_until_parked();
            self.shell
                .update_in(self.cx, |shell, window, cx| shell.apply_stores(window, cx));
            self.cx.run_until_parked();
        }

        fn connected(&mut self) {
            self.frames_until("the connection", |shell, _| {
                shell.copies.settings.value().connection == ConnectionStatus::Connected
            });
        }

        fn say(&mut self, bytes: &[u8]) {
            self.server.write_all(bytes).unwrap();
        }

        fn wait_for_chat(&mut self, text: &'static str) {
            self.frames_until(text, move |shell, cx| {
                shell.chat.read(cx).newest_text().as_deref() == Some(text)
            });
        }

        /// What the server reads, up to the end of a line.
        fn heard(&mut self) -> String {
            let mut heard = Vec::new();
            let mut byte = [0];
            while byte != *b"\n" {
                self.server.read_exact(&mut byte).unwrap();
                if byte == [IAC] {
                    self.server.read_exact(&mut [0, 0]).unwrap();
                } else {
                    heard.push(byte[0]);
                }
            }
            String::from_utf8_lossy(&heard).trim().to_owned()
        }

        fn chat_lines(&mut self) -> Vec<String> {
            self.shell.read_with(self.cx, |shell, cx| {
                let chat = shell.chat.read(cx);
                (0..chat.len()).filter_map(|n| chat.line_text(n)).collect()
            })
        }
    }

    #[gpui::test]
    fn typing_goes_to_the_input_and_return_echoes_it(cx: &mut TestAppContext) {
        cx.update(crate::input::bind_keys);
        let (_waker, woken) = unbounded();
        let (shell, cx) = cx.add_window_view(|window, cx| Shell::new(None, woken, window, cx));
        let chat = shell.read_with(cx, |shell, _| shell.chat());
        chat.update(cx, |chat, cx| {
            (0..1_000).for_each(|_| chat.echo_unsent("a line", cx));
            chat.scroll_back(px(400.), cx);
        });
        cx.run_until_parked();
        let reading = chat.read_with(cx, |chat, _| chat.scroll_offset());

        cx.simulate_input("look north");
        assert_eq!(
            chat.read_with(cx, |chat, _| chat.scroll_offset()),
            reading,
            "typing must not move the chat"
        );

        cx.simulate_keystrokes("enter");
        assert_eq!(
            chat.read_with(cx, |chat, _| chat.newest_text()).as_deref(),
            Some("look north")
        );
    }

    #[gpui::test]
    fn a_player_reads_the_server_and_sends_commands(cx: &mut TestAppContext) {
        let mut played = play(cx);
        played.say(b"Welcome to the fake server.\r\n");
        played.wait_for_chat("Welcome to the fake server.");

        played.cx.simulate_input("look");
        played.cx.simulate_keystrokes("enter");
        assert_eq!(played.heard(), "look");
        played.wait_for_chat("look");
    }

    #[gpui::test]
    fn a_wake_waits_for_the_frame_which_applies_one_batch(cx: &mut TestAppContext) {
        let mut played = play(cx);
        played.connected();
        played.say(b"one\r\ntwo\r\nthree\r\n");
        let lines_in_core = |shell: &Shell| {
            let stores = shell.game.as_ref().unwrap().session.stores();
            stores.read(|terminal: &Versioned<TerminalState>| terminal.value().lines().len())
        };
        let started = Instant::now();
        while played.shell.read_with(played.cx, |s, _| lines_in_core(s)) < 3 {
            assert!(started.elapsed() < WAIT, "timed out waiting for the lines");
            sleep(Duration::from_millis(1));
        }

        played.wake.unbounded_send(()).unwrap();
        played.cx.run_until_parked();
        assert_eq!(played.chat_lines().len(), 0, "no frame has run");
        played.frame();
        assert_eq!(played.chat_lines(), ["one", "two", "three"]);
    }

    #[gpui::test]
    fn vitals_and_the_connection_reach_the_status_line(cx: &mut TestAppContext) {
        let mut played = play(cx);
        played.say(&[IAC, WILL, GMCP]);
        played.say(&[IAC, SB, GMCP]);
        played.say(br#"Char.Vitals {"hp":11,"maxhp":30}"#);
        played.say(&[IAC, SE]);
        played.frames_until("the vitals", |shell, cx| {
            shell
                .status
                .read(cx)
                .lines()
                .first()
                .is_some_and(|line| line == "Connected to 127.0.0.1  hp 11  maxhp 30")
        });
    }

    #[gpui::test]
    fn a_password_is_masked_sent_and_not_kept(cx: &mut TestAppContext) {
        let mut played = play(cx);
        played.say(&[IAC, WILL, ECHO]);
        played.say(b"Password:\r\n");
        played.frames_until("echo off", |shell, _| {
            shell.copies.settings.value().echo_off
        });

        played.cx.simulate_input("hunter2");
        played.cx.simulate_keystrokes("enter");
        assert_eq!(played.heard(), "hunter2");

        played.say(&[IAC, WONT, ECHO]);
        played.frames_until("echo on", |shell, _| {
            !shell.copies.settings.value().echo_off
        });
        played.cx.simulate_input("look");
        played.cx.simulate_keystrokes("enter");
        assert_eq!(played.heard(), "look");
        played.wait_for_chat("look");
        assert!(!played.chat_lines().iter().any(|l| l.contains("hunter2")));
        played.shell.read_with(played.cx, |shell, _| {
            let session = &shell.game.as_ref().unwrap().session;
            assert_eq!(session.older("", "").as_deref(), Some("look"));
            assert_eq!(session.older("look", ""), None, "only look is in history");
        });
    }

    #[gpui::test]
    fn leaving_closes_the_connection_once(cx: &mut TestAppContext) {
        let mut played = play(cx);
        played.connected();
        let saved = played.shell.update(played.cx, |shell, cx| shell.leave(cx));
        played.cx.run_until_parked();
        saved.expect("the first leave").detach();
        played.cx.run_until_parked();

        let mut rest = Vec::new();
        played.server.read_to_end(&mut rest).unwrap();
        let again = played.shell.update(played.cx, |shell, cx| shell.leave(cx));
        assert!(again.is_none());
    }
}
