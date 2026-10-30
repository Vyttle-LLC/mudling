//! The window's root view, a thin shell over a [`Session`]: a centered chat
//! column with the floating command bar at its foot, between two [`Dock`]s.
//!
//! The bar looks like it floats, but the chat ends where it begins, so
//! however tall the bar grows it can't cover the newest line.
//!
//! The UI thread only subscribes, applies and draws. The session's waker
//! comes from a core thread; each wake asks for the next frame, and that
//! frame drains the stores once, so updates apply at most once per frame.

use crate::assets::UI_FONT;
use crate::chat::{COLUMN_WIDTH, Chat};
use crate::dock::{Dock, DockState, Side, Slot};
use crate::hud::Hud;
use crate::input::{CommandInput, InputEvent};
use crate::status::{StatusLines, connection_text, lines_for};
use crate::theme::Theme;
use futures::StreamExt;
use futures::channel::mpsc::UnboundedReceiver;
use gpui::{
    AnyView, App, Context, Entity, Focusable, KeyBinding, Subscription, Task, Window, actions, div,
    prelude::*,
};
use mudling_core::completion::Completion;
use mudling_core::plugin::PluginId;
use mudling_core::session::Session;
use mudling_core::store::{ConnectionStatus, SettingsState, Versioned, VitalsState};
use mudling_core::telnet::Bytes;
use tempfile::TempDir;

actions!(shell, [ToggleLeftPanel, ToggleRightPanel, FocusCommandBar]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new(secondary!("1"), ToggleLeftPanel, None),
        KeyBinding::new(secondary!("2"), ToggleRightPanel, None),
        KeyBinding::new(secondary!("l"), FocusCommandBar, None),
    ]);
}

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
    hud: Entity<Hud>,
    input: Entity<CommandInput>,
    left_dock: Entity<Dock>,
    right_dock: Entity<Dock>,
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
        let typing = cx.subscribe_in(&input, window, |shell, _, event, window, cx| {
            shell.on_typing(event, window, cx);
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
            hud: cx.new(|_| Hud::default()),
            input,
            left_dock: cx.new(|_| Dock::new(Side::Left)),
            right_dock: cx.new(|_| Dock::new(Side::Right)),
            _subscriptions: [typing, appearance, quitting],
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
        let settings = self.copies.settings.value();
        let lines = lines_for(settings, &game.enabled);
        let connection = connection_text(settings, game.replay_folder.is_some()).into();
        self.status.update(cx, |status, cx| status.show(lines, cx));
        self.hud.update(cx, |hud, cx| {
            hud.show(connection, self.copies.vitals.value(), cx);
        });
    }

    fn on_typing(&mut self, event: &InputEvent, window: &mut Window, cx: &mut Context<Shell>) {
        if let InputEvent::Submitted(command) = event {
            self.submit(command, cx);
            return;
        }
        let Some(game) = &self.game else { return };
        let at_the_end = |line: String| Completion {
            cursor: line.len(),
            line,
        };
        let shown = match event {
            InputEvent::Submitted(_) => None,
            InputEvent::Older { line, prefix } => game.session.older(line, prefix).map(at_the_end),
            InputEvent::Newer { line } => game.session.newer(line).map(at_the_end),
            InputEvent::Complete { line, cursor } => game.session.complete(line, *cursor),
        };
        if let Some(Completion { line, cursor }) = shown {
            self.input
                .update(cx, |input, cx| input.show(line, cursor, window, cx));
        }
    }

    /// Sends what was typed, a command to each line; the session echoes them,
    /// unless it's a password. With no server connected they go nowhere, so
    /// the chat shows them here.
    fn submit(&mut self, typed: &str, cx: &mut Context<Shell>) {
        let settings = self.copies.settings.value();
        let connected = settings.connection == ConnectionStatus::Connected;
        for command in typed.split('\n') {
            match &self.game {
                Some(game) if connected && game.replay_folder.is_none() => {
                    game.session.send(command);
                }
                _ if settings.echo_off => {}
                _ => self
                    .chat
                    .update(cx, |chat, cx| chat.echo_unsent(command, cx)),
            }
        }
    }

    /// Puts `view` in a panel slot: how later tickets fill the panels.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn mount(&self, slot: Slot, view: impl Into<AnyView>, cx: &mut Context<Shell>) {
        let view = view.into();
        self.dock(slot.side())
            .update(cx, |dock, cx| dock.mount(slot, view, cx));
    }

    /// The dock on `side`, for what opens and closes it, such as the title
    /// bar's toggles.
    pub fn dock(&self, side: Side) -> &Entity<Dock> {
        match side {
            Side::Left => &self.left_dock,
            Side::Right => &self.right_dock,
        }
    }

    /// Opens both panels: the frame budget holds with everything open.
    pub fn open_panels(&self, cx: &mut Context<Shell>) {
        for side in [Side::Left, Side::Right] {
            self.dock(side)
                .update(cx, |dock, cx| dock.move_to(DockState::Open, cx));
        }
    }

    fn toggle_dock(&self, side: Side, cx: &mut Context<Shell>) {
        self.dock(side).update(cx, |dock, cx| dock.toggle(cx));
    }

    #[cfg(test)]
    pub fn show_sample_status(
        &self,
        connection: &'static str,
        vitals: &VitalsState,
        cx: &mut Context<Shell>,
    ) {
        self.hud
            .update(cx, |hud, cx| hud.show(connection.into(), vitals, cx));
    }

    #[cfg(test)]
    pub fn type_sample(&self, text: &str, window: &mut Window, cx: &mut Context<Shell>) {
        self.input.update(cx, |input, cx| {
            input.show(text.to_owned(), text.len(), window, cx)
        });
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
        let theme = *cx.global::<Theme>();
        let command_bar = div()
            .debug_selector(|| "command-bar".to_owned())
            .rounded_xl()
            .border_1()
            .border_color(theme.border)
            .bg(theme.surface_raised)
            .shadow(theme.floating_shadow())
            .child(self.hud.clone())
            .child(self.input.clone());
        let column = div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .items_center()
            .child(
                div()
                    .debug_selector(|| "chat-area".to_owned())
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .max_w(COLUMN_WIDTH)
                    .px_8()
                    .pt_4()
                    .child(self.chat.clone()),
            )
            .child(
                div()
                    .w_full()
                    .max_w(COLUMN_WIDTH)
                    .px_4()
                    .pt_3()
                    .pb_4()
                    .child(self.status.clone())
                    .child(command_bar),
            );
        div()
            .size_full()
            .relative()
            .flex()
            .bg(theme.background)
            .text_color(theme.text)
            .font_family(UI_FONT)
            .on_action(cx.listener(|shell, _: &ToggleLeftPanel, _, cx| {
                shell.toggle_dock(Side::Left, cx);
            }))
            .on_action(cx.listener(|shell, _: &ToggleRightPanel, _, cx| {
                shell.toggle_dock(Side::Right, cx);
            }))
            .on_action(cx.listener(|shell, _: &FocusCommandBar, window, cx| {
                window.focus(&shell.input.focus_handle(cx), cx);
            }))
            .child(self.left_dock.clone())
            .child(column)
            .child(self.right_dock.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::LINE_HEIGHT;
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
            let hud = shell.hud.read(cx);
            hud.connection() == "Connected to 127.0.0.1" && hud.values() == ["11/30"]
        });
    }

    #[gpui::test]
    fn up_recalls_a_sent_command_and_tab_completes_a_word(cx: &mut TestAppContext) {
        let mut played = play(cx);
        played.connected();
        played.cx.simulate_input("get lantern");
        played.cx.simulate_keystrokes("enter");
        assert_eq!(played.heard(), "get lantern");
        let typed = |played: &mut Played| {
            played.shell.read_with(played.cx, |shell, cx| {
                shell.input.read(cx).text().to_owned()
            })
        };

        played.cx.simulate_keystrokes("up");
        assert_eq!(typed(&mut played), "get lantern");
        played.cx.simulate_keystrokes("down");
        assert_eq!(typed(&mut played), "");

        played.cx.simulate_input("drop la");
        played.cx.simulate_keystrokes("tab");
        assert_eq!(typed(&mut played), "drop lantern");
    }

    #[gpui::test]
    fn each_line_of_a_multi_line_input_goes_out_as_a_command(cx: &mut TestAppContext) {
        let mut played = play(cx);
        played.connected();
        played.cx.simulate_input("look");
        played.cx.simulate_keystrokes("shift-enter");
        played.cx.simulate_input("inv");
        played.cx.simulate_keystrokes("enter");
        assert_eq!(played.heard(), "look");
        assert_eq!(played.heard(), "inv");
    }

    #[gpui::test]
    fn history_and_completion_stay_out_of_a_password(cx: &mut TestAppContext) {
        let mut played = play(cx);
        played.connected();
        played.cx.simulate_input("look");
        played.cx.simulate_keystrokes("enter");
        assert_eq!(played.heard(), "look");
        played.say(&[IAC, WILL, ECHO]);
        played.frames_until("echo off", |shell, _| {
            shell.copies.settings.value().echo_off
        });

        played.cx.simulate_keystrokes("up");
        played.cx.simulate_input("lo");
        played.cx.simulate_keystrokes("tab shift-enter");
        played.shell.read_with(played.cx, |shell, cx| {
            assert_eq!(shell.input.read(cx).text(), "lo");
        });
    }

    #[gpui::test]
    fn panel_shortcuts_toggle_the_docks_and_leave_typing_alone(cx: &mut TestAppContext) {
        cx.update(|cx| {
            crate::input::bind_keys(cx);
            bind_keys(cx);
            cx.set_reduce_motion(true);
        });
        let (_waker, woken) = unbounded();
        let (shell, cx) = cx.add_window_view(|window, cx| Shell::new(None, woken, window, cx));
        let state = |shell: &Entity<Shell>, side, cx: &mut VisualTestContext| {
            shell.read_with(cx, |shell, cx| shell.dock(side).read(cx).state())
        };

        cx.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-1"
        } else {
            "ctrl-1"
        });
        assert_eq!(state(&shell, Side::Left, cx), DockState::Open);
        assert_eq!(state(&shell, Side::Right, cx), DockState::Hidden);

        cx.simulate_input("look");
        cx.simulate_keystrokes("enter");
        let chat = shell.read_with(cx, |shell, _| shell.chat());
        assert_eq!(
            chat.read_with(cx, |chat, _| chat.newest_text()).as_deref(),
            Some("look")
        );
    }

    #[gpui::test]
    fn the_chat_stops_above_the_command_bar_as_it_grows(cx: &mut TestAppContext) {
        cx.update(crate::input::bind_keys);
        let (_waker, woken) = unbounded();
        let (shell, cx) = cx.add_window_view(|window, cx| Shell::new(None, woken, window, cx));
        let chat = shell.read_with(cx, |shell, _| shell.chat());
        chat.update(cx, |chat, cx| {
            (0..200).for_each(|_| chat.echo_unsent("a line", cx));
        });
        cx.run_until_parked();

        cx.simulate_input("one");
        cx.simulate_keystrokes("shift-enter");
        cx.simulate_input("two");
        cx.simulate_keystrokes("shift-enter");
        cx.simulate_input("three");
        cx.run_until_parked();

        let chat_bottom = cx.debug_bounds("chat-area").unwrap().bottom();
        let bar = cx.debug_bounds("command-bar").unwrap();
        assert!(chat_bottom <= bar.top(), "{chat_bottom:?} over {bar:?}");
        assert!(
            bar.size.height >= LINE_HEIGHT * 3.,
            "the bar grows: {:?}",
            bar.size
        );
        assert!(
            chat.read_with(cx, |chat, _| chat.follows_newest()),
            "the newest line stays in view as the bar grows"
        );
    }

    #[gpui::test]
    fn a_later_ticket_fills_a_slot_through_the_shell(cx: &mut TestAppContext) {
        struct Filled;
        impl Render for Filled {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                div()
                    .debug_selector(|| "filled-slot".to_owned())
                    .child("filled")
            }
        }
        cx.update(|cx| cx.set_reduce_motion(true));
        let (_waker, woken) = unbounded();
        let (shell, cx) = cx.add_window_view(|window, cx| Shell::new(None, woken, window, cx));
        shell.update(cx, |shell, cx| {
            let filled = cx.new(|_| Filled);
            shell.mount(Slot::Room, filled, cx);
            shell.open_panels(cx);
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("filled-slot").is_some());
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
