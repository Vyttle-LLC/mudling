//! The measuring mode (`--measure`): frame times while replaying.
//!
//! gpui times every frame it draws (layout, prepaint, paint and the hand-off
//! to the GPU) and prints `frame duration: …` to stderr when
//! `ZED_MEASUREMENTS=1`. The binary re-launches itself with that set, runs
//! the phases below in the child, and sorts the child's frame times by the
//! phase markers it prints on the same stream, so their order is exact:
//!
//! 1. `startup`: the window opens, the session replays the full scrollback
//!    into it, then it settles.
//! 2. `stream`: lines arrive at the replay pace, pinned to the newest line.
//! 3. `split`: the chat splits a page back, and lines arrive for as long as
//!    in `stream`, into the live pane with the history pane held still.
//! 4. `scroll`: the history pane scrolls back while lines still arrive, so
//!    every frame lays out lines it hasn't shown before, in both panes.
//! 5. `rewrap`: the right panel closes, leaving the column room to change
//!    width, and the left one is dragged back and forth while lines still
//!    arrive, so every frame wraps both panes to a new width.
//!
//! `--long-lines` measures all of this with lines that wrap.
//!
//! `--settings-open` runs all of this with the settings sheet open, and
//! `--settings-section` with Data & sync or Vault open and filled.
//!
//! `--hud <where>` runs all of this with [`sample_hud`]'s HUD at that place,
//! its numbers changing on every frame, and a prompt on every read. `open` is
//! the attached row opened. `dragged` is the floating card, moved each frame
//! by what a mouse drag calls.
//!
//! `--find-open` opens the find bar as `stream` starts, with results showing
//! from then on: the newest opens in the history pane, so `stream` is split
//! too. The time from typing to the results goes in the report, to the frame.
//!
//! `--find-panel` brings the Find panel up as `stream` starts and types in it
//! from then on, a key every few frames, so its results change throughout. The
//! slowest search goes in the report.
//!
//! The child is paced by gpui's frame callbacks, not by timers: on a busy
//! machine timers fire late and would thin out the frames being measured.
//! Lines are due by the clock, so the pace stays exact either way.
//!
//! Lines go through `Session::replay` on the core thread, and reach the chat
//! the way a server's do: as a store diff, applied on a later frame.
//!
//! The Room panel stays filled throughout. A replay's logs fill it with their
//! [`fullest_room`]: a paced replay is many short connections with no GMCP in
//! them, so the room is read from the logs' GMCP once, before the window
//! opens. Otherwise it's [`sample_room`], which is made up.
//!
//! A check runs beside whatever the player is doing: the app never activates,
//! and on macOS it has no Dock icon and its window sits off the screen (see
//! `macos::show_unseen`). If its frames stop, the check ends with an error
//! rather than wait for them ([`fail_if_frames_stop`]).
//!
//! `--sample-map <rooms>` gives the session a made-up map of that many rooms
//! ([`save_sample_map`]), with a mob in every tenth and the player in the
//! first, so `rss_mb_after_load` can be held against the PRD's 200 MB with a
//! 10,000-room map.

use crate::chat::{COLUMN_WIDTH, Chat};
use crate::dock::{Dock, DockState, Side, Slot};
use crate::find_panel::FindPanel;
use crate::hud::HudPlacement;
use crate::map_view::harness::{noted_map, versioned};
use crate::notes::{Host, NoteTarget};
use crate::settings::Section;
use crate::shell::Shell;
use crate::stream;
use gpui::{
    ActivityGuard, AnyWindowHandle, App, Context, Entity, EntityInputHandler, Window, point, px,
};
use mudling_core::defaults::hud::Vital;
use mudling_core::map::{CURRENT_ROOM, FindStores, LiveMap, Mark, MarkChange, PlainRules};
use mudling_core::persist::{self, Batch, Database, Exit, Link, Meta, Room, Sighting, Timestamp};
use mudling_core::plugin::luau::Value;
use mudling_core::replay::ReplayFeed;
use mudling_core::session::{Engine, SessionConfig};
use mudling_core::stats::Summary;
use mudling_core::store::{
    Affect, CHARACTER_INVENTORY, CHARACTER_LEVEL, CHARACTER_NAME, CHARACTER_POSITION,
    CharactersState, Item, MapState, MobsState, Occupant, RoomContents, RoomState, StoreHub,
    Target, TargetState, Versioned, VitalsState,
};
use mudling_core::telnet::gmcp;
use mudling_core::vault::EntryKey;
use serde::Serialize;
use std::cell::Cell;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

const MARKER: &str = "mudling-measure: ";
const FRAME_PREFIX: &str = "frame duration: ";
const FRAME_BUDGET_US: f64 = 8_000.0;
const SETTLE: Duration = Duration::from_secs(1);
/// Longer than a busy machine holds up the display, and than a quit waits.
const FRAME_TIMEOUT: Duration = Duration::from_secs(15);
const NO_FRAMES: &str = "no frames for 15 seconds, so this check can't finish: macOS sends a \
                         window none while the screen is locked or asleep";
const NO_FRAMES_EXIT: i32 = 3;
const SCROLL_DURATION: Duration = Duration::from_secs(5);
const REWRAP_DURATION: Duration = Duration::from_secs(5);
/// How far the panel is dragged: about what the column has to give.
const REWRAP_DRAG: f32 = 60.;
/// Scrolled per display frame: about a line and a half.
const SCROLL_STEP: f32 = 30.;
/// How far back the split opens: more than a window of history.
const SPLIT_DISTANCE: f32 = 2_000.;
/// How far the HUD's sample changes before it starts again, and how many
/// pixels the card is dragged.
const CARD_SWAY: u64 = 400;
/// Common in any English text, so results are on screen and many.
const FIND_TEXT: &str = "the";
/// What's typed in the Find panel, over and over: the sample map's rooms and
/// mobs, from every room down to a few.
const FIND_PANEL_TYPING: [&str; 8] = ["p", "pa", "pat", "path", "path 1", "path 12", "wolf", ""];
/// Frames between keys: a quick typist, on a 120 Hz display.
const FRAMES_PER_KEY: u32 = 8;
/// Every this many rooms of the sample map, a mob was seen.
const ROOMS_PER_MOB: i64 = 10;
/// About as long as a real room's description.
const SAMPLE_DESCRIPTION: &str = "Tall grass sways on either side of the path, which bends \
    away between low stones furred with moss. A cold wind comes down from the hills to the \
    north, smelling of rain and old smoke, and somewhere ahead a bird calls twice and stops.";

/// Notes on a square of this side, for the Notes tab: 400 of them.
const NOTED_SIDE: i64 = 20;
const SAMPLE_NOTEPAD: &str = "Goals\n- Reach level 7.\n\nPeople\n- The healer in the square.";
/// Frames between keys while typing a note: ten keys a second at 60 Hz.
const FRAMES_PER_NOTE_KEY: u64 = 6;

/// A filled Room panel's room, what the map remembers and what's in it.
pub fn sample_room() -> (RoomState, MobsState, RoomContents) {
    let mut map = LiveMap::new(Box::new(PlainRules));
    let info = r#"{"num": 1, "name": "A Stone Hall", "zone": "The Old Town",
        "exits": {"n": 2, "s": 3, "e": "closed"}}"#;
    map.room_info(info, Timestamp(0));
    map.note_sightings(&["(Marked) a grey wolf".to_owned()], Timestamp(0));
    let room = map
        .commit(Timestamp(0))
        .and_then(|commit| commit.change.rooms.first().map(Arc::clone));
    let mobs = MobsState::default().with_changes(&[mudling_core::map::MarkChange {
        name: "wolf".to_owned(),
        mark: mudling_core::map::Mark::Aggressive,
        set: true,
    }]);
    let item = |name: &str, target: &str, count| Item {
        name: name.into(),
        target: target.to_owned(),
        count,
    };
    let contents = RoomContents {
        occupants: vec![Occupant {
            name: "A traveller resting by the wall".into(),
            target: Some("traveller".to_owned()),
        }],
        items: vec![
            item("a worn leather satchel with a strap", "satchel", 3),
            item("some coins", "coins", 4),
            item("a blanket", "blanket", 2),
            item("a stone", "stone", 2),
            item("some rope", "rope", 1),
        ],
    };
    let room = RoomState {
        room,
        whereabouts: Default::default(),
        contents: contents.clone(),
    };
    (room, mobs, contents)
}

/// A small made-up world for the Find panel: rooms with a walk between them,
/// one out of reach, mobs seen and marked, and items at hand.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn sample_find() -> FindStores {
    let now = Timestamp(0);
    let mut live = LiveMap::new(Box::new(PlainRules));
    live.room_info(
        r#"{"num": 9, "name": "A Hermit's Hut", "zone": "The Far Woods", "exits": {}}"#,
        now,
    );
    live.room_info(
        r#"{"num": 3, "name": "The North Gate", "zone": "The Old Town", "exits": {"s": 2}}"#,
        now,
    );
    live.note_sightings(&["A grey wolf is here, howling.".to_owned()], now);
    live.sent("south", now);
    live.room_info(
        r#"{"num": 2, "name": "The Market Square", "zone": "The Old Town",
            "exits": {"n": 3, "s": 1}}"#,
        now,
    );
    live.note_sightings(&["A healer tends her herbs.".to_owned()], now);
    live.sent("south", now);
    live.room_info(
        r#"{"num": 1, "name": "A Stone Hall", "zone": "The Old Town", "exits": {"n": 2}}"#,
        now,
    );
    let mut map = Versioned::default();
    if let Some(commit) = live.commit(now) {
        map.write(&commit.change);
    }
    let mark = |mark, name: &str| MarkChange {
        name: name.to_owned(),
        mark,
        set: true,
    };
    let mut mobs: Versioned<MobsState> = Versioned::default();
    mobs.write(&MobsState::default().with_changes(&[
        mark(Mark::Aggressive, "wolf"),
        mark(Mark::Gear, "a pale king"),
    ]));
    let (_, _, contents) = sample_room();
    let map_state: &MapState = map.value();
    let mut room: Versioned<RoomState> = Versioned::default();
    room.write(&RoomState {
        room: map_state.room(1).cloned(),
        contents,
        ..RoomState::default()
    });
    let mut characters: Versioned<CharactersState> = Versioned::default();
    characters.write(&CharactersState(BTreeMap::from([
        (format!("{CHARACTER_INVENTORY}.count"), Value::Number(1.)),
        (
            format!("{CHARACTER_INVENTORY}.1.name"),
            Value::Text("a brass lantern".to_owned()),
        ),
    ])));
    FindStores::of(map, mobs, room, characters)
}

/// The room with the most in it across `logs`, their GMCP read through an
/// engine as one connection would be. `None` if they list nothing in a room.
pub fn fullest_room(logs: &[PathBuf]) -> Option<RoomState> {
    let size = |room: &RoomState| room.contents.occupants.len() + room.contents.items.len();
    let folder = tempfile::tempdir().ok()?;
    let hub = Arc::new(StoreHub::new(|| {}));
    let config = SessionConfig::new(folder.path(), "measure.example", 4000);
    let mut engine = Engine::open(config, Arc::clone(&hub), Arc::default()).ok()?;
    engine.start_connection().ok()?;
    let mut fullest: Option<RoomState> = None;
    for log in logs {
        let bytes = std::fs::read(log).ok()?;
        let text = String::from_utf8_lossy(&bytes);
        for message in text.lines().filter_map(|line| line.strip_prefix("[GMCP] ")) {
            let (package, data) = message.split_once(' ').unwrap_or((message, ""));
            let mut frame = Vec::new();
            gmcp::encode(package, data, &mut frame);
            engine.receive(frame.into(), Timestamp(0)).ok()?;
            engine.settle(Timestamp(0));
            drop(engine.take_output());
            let room = hub.read(|room: &Versioned<RoomState>| room.value().clone());
            if size(&room) > fullest.as_ref().map_or(0, size) {
                fullest = Some(room);
            }
        }
    }
    let room = fullest?;
    mark(&format!(
        "info room_occupants={}",
        room.contents.occupants.len()
    ));
    mark(&format!("info room_items={}", room.contents.items.len()));
    Some(room)
}

/// Chips such as a player keeps. Made up for measuring.
pub fn sample_chips() -> Vec<String> {
    ["look", "inv", "score", "kill spirit", "sleep"]
        .map(str::to_owned)
        .into()
}

pub fn fill_hotbar(shell: &Shell, cx: &mut App) {
    shell
        .hotbar()
        .update(cx, |hotbar, cx| hotbar.show(&sample_chips(), cx));
}

pub fn fill_room_panel(shell: &Shell, room: Option<RoomState>, cx: &mut App) {
    let (sample, mobs, _) = sample_room();
    let room = room.unwrap_or(sample);
    shell.room_panel().update(cx, |panel, cx| {
        panel.show_room(&room, &mobs, cx);
        panel.show_contents(room.contents.clone(), cx);
    });
}

/// The Notes tab open on 400 made-up notes, with the editor on the first.
pub fn open_notes(shell: &mut Shell, window: &mut Window, cx: &mut Context<Shell>) {
    let notes = shell.notes();
    notes.update(cx, |notes, cx| {
        notes.use_map(versioned(noted_map(NOTED_SIDE)), cx);
        notes.show_server("mud.example", SAMPLE_NOTEPAD, cx);
    });
    shell
        .dock(Side::Right)
        .update(cx, |dock, cx| dock.select(Slot::Notes, cx));
    notes.update(cx, |notes, cx| {
        notes.edit(NoteTarget::Room(0), Host::NotesTab, window, cx);
    });
}

/// Where a measured run puts the HUD.
#[derive(Clone, Copy, PartialEq)]
pub enum HudRun {
    Attached,
    Open,
    Floating,
    Dragged,
    TopStrip,
    Hidden,
}

impl HudRun {
    pub fn named(name: &str) -> Option<HudRun> {
        match name {
            "attached" => Some(HudRun::Attached),
            "open" => Some(HudRun::Open),
            "floating" => Some(HudRun::Floating),
            "dragged" => Some(HudRun::Dragged),
            "strip" => Some(HudRun::TopStrip),
            "hidden" => Some(HudRun::Hidden),
            _ => None,
        }
    }

    /// Puts the HUD there and keeps it on the sample: each read is a connection
    /// of its own, and its stores would otherwise replace the sample.
    pub fn put(self, shell: &mut Shell, window: &mut Window, cx: &mut Context<Shell>) {
        shell.hold_hud();
        let placement = match self {
            HudRun::Attached | HudRun::Open => HudPlacement::Attached,
            HudRun::Floating | HudRun::Dragged => HudPlacement::Floating,
            HudRun::TopStrip => HudPlacement::TopStrip,
            HudRun::Hidden => HudPlacement::Hidden,
        };
        shell.place_hud(placement, self == HudRun::Open, window, cx);
    }
}

/// A character in a fight, made up, whose hit points fall by `lost`.
pub fn sample_hud(lost: i64) -> (VitalsState, CharactersState, TargetState) {
    let vitals = VitalsState(BTreeMap::from([
        (Vital::Hp, 3_120 - lost),
        (Vital::MaxHp, 4_300),
        (Vital::Mana, 1_040),
        (Vital::MaxMana, 1_800),
        (Vital::Moves, 810),
        (Vital::MaxMoves, 900),
        (Vital::ToNextLevel, 1_240),
        (Vital::MaxToNextLevel, 3_760),
    ]));
    let affect = |name: &str, detail: &str| Affect {
        name: name.to_owned(),
        detail: detail.to_owned(),
    };
    let mut characters = CharactersState::listing(&[
        affect("sanctuary", "for 12 hours"),
        affect("haste", "for 4 hours"),
        affect("bless", "for a while"),
    ]);
    characters.0.extend([
        (CHARACTER_NAME.to_owned(), Value::Text("Marrow".to_owned())),
        (CHARACTER_LEVEL.to_owned(), Value::Number(42.)),
        (
            CHARACTER_POSITION.to_owned(),
            Value::Text("Fight".to_owned()),
        ),
    ]);
    let target = TargetState(Some(Target {
        name: "the crazed goblin".to_owned(),
        health: Some(41),
        max_health: Some(100),
        level: Some(38),
    }));
    (vitals, characters, target)
}

/// Saves a made-up square grid of `rooms` rooms as `host`'s map, each room
/// linked to its neighbours and worded like a real one, with the player in the
/// first, so a replay can measure a large map. Its data folder is a throwaway
/// one.
pub fn save_sample_map(folder: &Path, host: &str, rooms: i64) -> Result<(), persist::Error> {
    let side = (rooms as f64).sqrt().ceil() as i64;
    let updated = Timestamp::now();
    let mut batch = Batch::new();
    batch.upsert(Meta {
        key: CURRENT_ROOM.to_owned(),
        value: 1.to_string(),
        updated,
    });
    for id in 1..=rooms {
        if id % ROOMS_PER_MOB == 0 {
            batch.upsert(Sighting {
                room_id: id,
                mob: format!("A grey wolf number {id} is here, howling."),
                updated,
            });
        }
        batch.upsert(Room {
            id,
            name: format!("A winding path, part {id}"),
            area: format!("Area {}", id / 250),
            environment: "field".to_owned(),
            description: format!("{SAMPLE_DESCRIPTION} The stones here are marked {id}."),
            x: (id - 1) % side,
            y: (id - 1) / side,
            z: 0,
            building: None,
            tangled: false,
            maze: false,
            updated,
        });
        let mut link = |direction: &str, back: &str, to: i64| {
            for (room_id, way, to_room) in [(id, direction, to), (to, back, id)] {
                let direction = way.to_owned();
                batch.upsert(Exit {
                    room_id,
                    direction: direction.clone(),
                    state: "open".to_owned(),
                    updated,
                });
                batch.upsert(Link {
                    room_id,
                    direction,
                    to_room,
                    updated,
                });
            }
        };
        if id % side != 0 && id < rooms {
            link("east", "west", id + 1);
        }
        if id + side <= rooms {
            link("north", "south", id + side);
        }
    }
    let database = Database::open(&folder.join(persist::SERVERS_FOLDER), host)?;
    let writer = database.writer()?;
    writer.submit(batch)?;
    writer.flush()
}

/// Prints one marker for the measuring parent.
pub fn mark(message: &str) {
    eprintln!("{MARKER}{message}");
}

/// A marker the child printed, for a parent reading its stderr.
pub fn marker(line: &str) -> Option<&str> {
    line.strip_prefix(MARKER)
}

/// A frame time gpui printed.
pub fn frame_duration(line: &str) -> Option<Duration> {
    line.strip_prefix(FRAME_PREFIX)
        .and_then(parse_debug_duration)
}

pub fn mark_rss(label: &str) {
    if let Some(stats) = memory_stats::memory_stats() {
        mark(&format!(
            "info {label}={:.1}",
            stats.physical_mem as f64 / (1024.0 * 1024.0)
        ));
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Load,
    Settle,
    Stream,
    Split,
    Scroll,
    Rewrap,
}

struct Pacer {
    shell: Entity<Shell>,
    chat: Entity<Chat>,
    launched: Instant,
    feed: ReplayFeed,
    lines_per_second: f64,
    stream_for: Duration,
    hud: Option<HudRun>,
    phase: Phase,
    started: Instant,
    stream_started: Option<Instant>,
    lines_sent: u64,
    /// Frames the HUD has changed on.
    hud_frames: u64,
    find_open: bool,
    /// When the find text went in, until its results show.
    find_started: Option<Instant>,
    find_panel: Option<FindPanelTyping>,
    notes_open: bool,
    frames: u64,
    _no_app_nap: ActivityGuard,
}

/// Types in the Find panel through a measured run.
struct FindPanelTyping {
    panel: Entity<FindPanel>,
    frames: u32,
    keys: usize,
    /// When the last key went in, until its results show.
    typed_at: Option<Instant>,
    searches: u32,
    slowest: Duration,
    most_rows: usize,
}

impl FindPanelTyping {
    fn on_frame(&mut self, window: &mut Window, cx: &mut App) {
        let rows = self.panel.read(cx).rows_for_typed(cx);
        if let (Some(typed_at), Some(rows)) = (self.typed_at, rows) {
            self.typed_at = None;
            self.searches += 1;
            self.slowest = self.slowest.max(typed_at.elapsed());
            self.most_rows = self.most_rows.max(rows);
        }
        self.frames += 1;
        if self.frames.is_multiple_of(FRAMES_PER_KEY) {
            let text = FIND_PANEL_TYPING[self.keys % FIND_PANEL_TYPING.len()];
            self.keys += 1;
            self.typed_at = Some(Instant::now());
            self.panel
                .update(cx, |panel, cx| panel.type_text(text, window, cx));
        }
    }

    fn report(&self) {
        mark(&format!("info find_panel_searches={}", self.searches));
        mark(&format!("info find_panel_most_rows={}", self.most_rows));
        mark(&format!(
            "info find_panel_slowest_ms={:.2}",
            self.slowest.as_secs_f64() * 1e3
        ));
    }
}

impl Pacer {
    fn start_find(&mut self, window: &mut Window, cx: &mut App) {
        self.find_started = Some(Instant::now());
        self.shell
            .update(cx, |shell, cx| shell.open_find(window, cx));
        self.chat.update(cx, |chat, cx| {
            chat.type_in_find(FIND_TEXT, window, cx);
        });
    }

    /// Reports the search once it's done.
    fn note_find_results(&mut self, cx: &App) {
        let Some(count) = self.chat.read(cx).find_count() else {
            return;
        };
        if let Some(started) = self.find_started.take() {
            mark(&format!(
                "info find_ms={:.2}",
                started.elapsed().as_secs_f64() * 1e3
            ));
            mark(&format!("info find_results={count}"));
        }
    }

    /// Runs once per display frame, before gpui decides whether to draw.
    fn on_frame(mut self, window: &mut Window, cx: &mut App) {
        if self.phase == Phase::Load {
            if !self.shell.read(cx).replay_finished() {
                window.on_next_frame(move |window, cx| self.on_frame(window, cx));
                return;
            }
            mark(&format!(
                "info load_ms={:.1}",
                self.launched.elapsed().as_secs_f64() * 1e3
            ));
            mark(&format!(
                "info scrollback_lines={}",
                self.chat.read(cx).len()
            ));
            mark_rss("rss_mb_after_load");
            self.phase = Phase::Settle;
            self.started = Instant::now();
        }
        let elapsed = self.started.elapsed();
        let split_at = SETTLE + self.stream_for;
        let scroll_at = split_at + self.stream_for;
        let phase = if elapsed < SETTLE {
            Phase::Settle
        } else if elapsed < split_at {
            Phase::Stream
        } else if elapsed < scroll_at {
            Phase::Split
        } else if elapsed < scroll_at + SCROLL_DURATION {
            Phase::Scroll
        } else if elapsed < scroll_at + SCROLL_DURATION + REWRAP_DURATION {
            Phase::Rewrap
        } else {
            mark_rss("rss_mb_end");
            if let Some(typing) = &self.find_panel {
                typing.report();
            }
            mark("phase end");
            window.remove_window();
            return;
        };
        if phase != self.phase {
            self.phase = phase;
            mark(match phase {
                Phase::Load | Phase::Settle => "phase startup",
                Phase::Stream => "phase stream",
                Phase::Split => "phase split",
                Phase::Scroll => "phase scroll",
                Phase::Rewrap => "phase rewrap",
            });
            if phase == Phase::Stream && self.find_open {
                self.start_find(window, cx);
            }
            if phase == Phase::Stream && self.find_panel.is_some() {
                self.shell
                    .update(cx, |shell, cx| shell.focus_find_panel(window, cx));
            }
            if phase == Phase::Split {
                self.chat
                    .update(cx, |chat, cx| chat.scroll_back(px(SPLIT_DISTANCE), cx));
            }
            if phase == Phase::Rewrap {
                self.dock(Side::Right, cx)
                    .update(cx, |dock, cx| dock.move_to(DockState::Hidden, cx));
            }
        }
        self.note_find_results(cx);
        if phase != Phase::Settle {
            let stream_started = *self.stream_started.get_or_insert_with(Instant::now);
            let due = (stream_started.elapsed().as_secs_f64() * self.lines_per_second) as u64;
            if due > self.lines_sent {
                let reads = stream::next_reads(&mut self.feed, due - self.lines_sent);
                let reads = match self.hud {
                    Some(_) => stream::with_a_prompt(reads),
                    None => reads,
                };
                self.shell.read(cx).replay(reads);
            }
            self.change_hud(window, cx);
            if let Some(typing) = &mut self.find_panel {
                typing.on_frame(window, cx);
            }
            self.type_a_key(window, cx);
            if phase == Phase::Rewrap {
                let there_and_back = (elapsed.as_secs_f32() % 2.0 - 1.0).abs();
                let window_width = window.viewport_size().width;
                let panel_width = window_width - COLUMN_WIDTH + px(REWRAP_DRAG * there_and_back);
                self.dock(Side::Left, cx).update(cx, |dock, cx| {
                    dock.resize_to(panel_width, window_width, cx);
                });
            }
            if phase == Phase::Scroll {
                self.chat
                    .update(cx, |chat, cx| chat.scroll_back(px(SCROLL_STEP), cx));
            }
            self.lines_sent = due;
        }
        window.on_next_frame(move |window, cx| self.on_frame(window, cx));
    }

    /// Types into the note, at about a typist's pace.
    fn type_a_key(&mut self, window: &mut Window, cx: &mut App) {
        self.frames += 1;
        if self.notes_open && self.frames.is_multiple_of(FRAMES_PER_NOTE_KEY) {
            let editor = self.shell.read(cx).notes().read(cx).editor().clone();
            editor.update(cx, |editor, cx| {
                editor.replace_text_in_range(None, "a", window, cx);
            });
        }
    }

    /// What a frame of a fight does to the HUD, and a mouse to the card.
    fn change_hud(&mut self, window: &mut Window, cx: &mut App) {
        let Some(hud) = self.hud else { return };
        self.hud_frames += 1;
        let sway = self.hud_frames % CARD_SWAY;
        let (vitals, characters, target) = sample_hud(sway as i64);
        self.shell.update(cx, |shell, cx| {
            shell.show_sample_hud(&vitals, &characters, &target, cx);
            if hud == HudRun::Dragged {
                let at = point(px(sway as f32), px(sway as f32));
                shell
                    .hud()
                    .update(cx, |hud, cx| hud.drag_card_to(at, window, cx));
            }
        });
    }

    fn dock(&self, side: Side, cx: &App) -> Entity<Dock> {
        self.shell.read(cx).dock(side).clone()
    }
}

/// How a measured run goes.
pub struct Run {
    pub lines_per_second: f64,
    pub stream_for: Duration,
    pub hud: Option<HudRun>,
    pub find_open: bool,
    pub find_panel: bool,
    pub notes_open: bool,
}

/// Calls `stalled` once `limit` has gone by since `last_frame`.
fn watch_frames(
    last_frame: Rc<Cell<Instant>>,
    limit: Duration,
    stalled: impl FnOnce() + 'static,
    cx: &mut App,
) {
    cx.spawn(async move |cx| {
        let executor = cx.background_executor().clone();
        while executor.now().duration_since(last_frame.get()) < limit {
            executor.timer(limit / 4).await;
        }
        stalled();
    })
    .detach();
}

/// Ends a check that can't finish, since its frames have stopped. Both kinds
/// of check are paced by frames, so without this they'd wait forever.
pub fn fail_if_frames_stop(window: AnyWindowHandle, cx: &mut App) {
    fn note_frames(last_frame: Rc<Cell<Instant>>, window: &mut Window, cx: &mut App) {
        last_frame.set(cx.background_executor().now());
        window.on_next_frame(move |window, cx| note_frames(last_frame, window, cx));
    }
    let last_frame = Rc::new(Cell::new(cx.background_executor().now()));
    let noted = Rc::clone(&last_frame);
    window
        .update(cx, |_, window, cx| note_frames(noted, window, cx))
        .expect("the window is open");
    let stalled = || {
        eprintln!("{NO_FRAMES}");
        std::process::exit(NO_FRAMES_EXIT);
    };
    watch_frames(last_frame, FRAME_TIMEOUT, stalled, cx);
}

/// Runs the measured phases in the child's window, then closes it, which
/// quits the app the way a player's close does.
pub fn drive(
    window: AnyWindowHandle,
    shell: Entity<Shell>,
    launched: Instant,
    feed: ReplayFeed,
    run: Run,
    cx: &mut App,
) {
    let find_panel = run.find_panel.then(|| FindPanelTyping {
        panel: shell.read(cx).find_panel(),
        frames: 0,
        keys: 0,
        typed_at: None,
        searches: 0,
        slowest: Duration::ZERO,
        most_rows: 0,
    });
    let pacer = Pacer {
        chat: shell.read(cx).chat(),
        shell,
        launched,
        feed,
        lines_per_second: run.lines_per_second,
        stream_for: run.stream_for,
        hud: run.hud,
        phase: Phase::Load,
        started: Instant::now(),
        stream_started: None,
        lines_sent: 0,
        hud_frames: 0,
        find_open: run.find_open,
        find_started: None,
        find_panel,
        notes_open: run.notes_open,
        frames: 0,
        _no_app_nap: cx
            .background_executor()
            .prevent_app_nap("MUDling measured replay"),
    };
    pacer.shell.update(cx, |shell, cx| {
        shell.open_panels(cx);
        fill_hotbar(shell, cx);
    });
    mark(&format!(
        "info window_active={}",
        u8::from(cx.active_window().is_some())
    ));
    window
        .update(cx, |_, window, _| {
            window.on_next_frame(move |window, cx| pacer.on_frame(window, cx))
        })
        .expect("the measured window is open");
}

#[derive(Serialize)]
struct FrameReport {
    os: &'static str,
    arch: &'static str,
    optimized: bool,
    /// Other work on the machine skews frame times; this says how busy it was.
    load_average_1m: Option<f64>,
    scope: &'static str,
    settings_open: bool,
    settings_section: Option<String>,
    hud: Option<String>,
    find_open: bool,
    find_panel: bool,
    notes_open: bool,
    lines_per_second: f64,
    stream_seconds: f64,
    time_to_first_frame_ms: Option<f64>,
    first_frame_ms: Option<f64>,
    frame_budget_us: f64,
    stream_within_budget: Option<bool>,
    split_within_budget: Option<bool>,
    phases: BTreeMap<String, Summary>,
    info: BTreeMap<String, f64>,
}

const SAMPLE_HOST: &str = "mud.example";
const SAMPLE_CHARACTER: &str = "Sample";

/// Opens the sheet on `section`, with Data & sync and Vault full of made-up
/// rows. They run on no repo and no vault, so nothing of the player's is read.
pub fn open_sample_section(
    shell: &mut Shell,
    section: Section,
    window: &mut Window,
    cx: &mut Context<Shell>,
) {
    shell.open_settings(window, cx);
    shell.settings_sheet().update(cx, |sheet, cx| {
        sheet.select(section, cx);
        sheet.show_sample_data(cx);
        let entries = [SAMPLE_CHARACTER, "Other"]
            .map(|character| EntryKey::new(SAMPLE_HOST, character))
            .to_vec();
        sheet.show_sample_vault(entries, 1, cx);
    });
}

/// Runs the app as a measured child and writes the report to `report_path`.
pub fn run_parent(
    child_args: &[String],
    lines_per_second: f64,
    stream_for: Duration,
    report_path: &Path,
) -> ExitCode {
    let exe = std::env::current_exe().expect("the running binary has a path");
    let load_average_1m = load_average();
    let launched = Instant::now();
    let mut child = match Command::new(exe)
        .args(child_args)
        .arg("--measured-run")
        .env("ZED_MEASUREMENTS", "1")
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            eprintln!("couldn't start the measured run: {error}");
            return ExitCode::FAILURE;
        }
    };

    let mut phase = "startup".to_owned();
    let mut frames: BTreeMap<String, Vec<Duration>> = BTreeMap::new();
    let mut info = BTreeMap::new();
    let mut time_to_first_frame = None;
    let mut first_frame = None;
    let stderr = BufReader::new(child.stderr.take().expect("stderr is piped"));
    for line in stderr.lines().map_while(Result::ok) {
        if let Some(duration) = line
            .strip_prefix(FRAME_PREFIX)
            .and_then(parse_debug_duration)
        {
            time_to_first_frame.get_or_insert_with(|| launched.elapsed());
            first_frame.get_or_insert(duration);
            frames.entry(phase.clone()).or_default().push(duration);
        } else if let Some(marker) = line.strip_prefix(MARKER) {
            if let Some(name) = marker.strip_prefix("phase ") {
                phase = name.to_owned();
            } else if let Some((key, value)) = marker
                .strip_prefix("info ")
                .and_then(|kv| kv.split_once('='))
                && let Ok(value) = value.parse()
            {
                info.insert(key.to_owned(), value);
            }
        } else if !line.starts_with("frame duration") {
            eprintln!("{line}");
        }
    }
    let status = child.wait();
    if phase != "end" {
        eprintln!("the measured run ended early ({status:?}) during phase {phase}");
        return ExitCode::FAILURE;
    }

    let phases: BTreeMap<String, Summary> = frames
        .iter()
        .filter_map(|(name, samples)| Some((name.clone(), Summary::of(samples)?)))
        .collect();
    let within_budget = |phase: &str| phases.get(phase).map(|s| s.p99_us <= FRAME_BUDGET_US);
    let report = FrameReport {
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        optimized: !cfg!(debug_assertions),
        load_average_1m,
        scope: "chat, command bar and both side panels open, the Room panel and the hotbar filled; \
                the chat split from the split phase on",
        settings_open: child_args
            .iter()
            .any(|arg| arg == "--settings-open" || arg == "--settings-section"),
        settings_section: child_args
            .iter()
            .position(|arg| arg == "--settings-section")
            .and_then(|flag| child_args.get(flag + 1))
            .cloned(),
        hud: child_args
            .iter()
            .position(|arg| arg == "--hud")
            .and_then(|flag| child_args.get(flag + 1))
            .cloned(),
        find_open: child_args.iter().any(|arg| arg == "--find-open"),
        find_panel: child_args.iter().any(|arg| arg == "--find-panel"),
        notes_open: child_args.iter().any(|arg| arg == "--notes-open"),
        lines_per_second,
        stream_seconds: stream_for.as_secs_f64(),
        time_to_first_frame_ms: time_to_first_frame.map(|d| d.as_secs_f64() * 1e3),
        first_frame_ms: first_frame.map(|d| d.as_secs_f64() * 1e3),
        frame_budget_us: FRAME_BUDGET_US,
        stream_within_budget: within_budget("stream"),
        split_within_budget: within_budget("split"),
        phases,
        info,
    };
    print_report(&report);
    let json = serde_json::to_string_pretty(&report).expect("the report serializes");
    if let Some(dir) = report_path.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    if let Err(error) = std::fs::write(report_path, json) {
        eprintln!("couldn't write {}: {error}", report_path.display());
        return ExitCode::FAILURE;
    }
    println!("report: {}", report_path.display());
    ExitCode::SUCCESS
}

fn print_report(report: &FrameReport) {
    if let Some(load) = report.load_average_1m {
        println!("load average (1 min) at start: {load:.1}");
    }
    if let (Some(ttff), Some(first)) = (report.time_to_first_frame_ms, report.first_frame_ms) {
        println!("time to first frame: {ttff:.0} ms (first frame took {first:.1} ms)");
    }
    println!(
        "settings sheet: {}",
        match (&report.settings_section, report.settings_open) {
            (Some(section), _) => section,
            (None, true) => "open",
            (None, false) => "closed",
        }
    );
    if let Some(hud) = &report.hud {
        println!("HUD: {hud}");
    }
    println!(
        "find bar: {}",
        if report.find_open { "open" } else { "closed" }
    );
    println!(
        "Find panel: {}",
        if report.find_panel {
            "typing"
        } else {
            "closed"
        }
    );
    println!(
        "Notes tab: {}",
        if report.notes_open {
            "typing"
        } else {
            "closed"
        }
    );
    for (phase, summary) in &report.phases {
        println!(
            "{phase:>8}: {} frames, p50 {:.2} ms, p99 {:.2} ms, max {:.2} ms",
            summary.samples,
            summary.p50_us / 1e3,
            summary.p99_us / 1e3,
            summary.max_us / 1e3
        );
    }
    for (key, value) in &report.info {
        println!("{key}: {value}");
    }
    for (phase, ok) in [
        ("stream", report.stream_within_budget),
        ("split", report.split_within_budget),
    ] {
        if let Some(ok) = ok {
            println!(
                "{phase} frame p99 ≤ 8 ms: {}",
                if ok { "ok" } else { "MISSED" }
            );
        }
    }
}

/// The one-minute load average, where the OS reports one without privileges.
pub fn load_average() -> Option<f64> {
    let text = if cfg!(target_os = "linux") {
        std::fs::read_to_string("/proc/loadavg").ok()?
    } else if cfg!(target_os = "macos") {
        let output = Command::new("sysctl")
            .args(["-n", "vm.loadavg"])
            .output()
            .ok()?;
        String::from_utf8(output.stdout).ok()?
    } else {
        return None;
    };
    text.split_whitespace().find_map(|field| field.parse().ok())
}

/// Parses a `Duration` as its `Debug` impl prints it, e.g. `1.25ms`.
fn parse_debug_duration(text: &str) -> Option<Duration> {
    let split = text.find(|c: char| !(c.is_ascii_digit() || c == '.'))?;
    let (number, unit) = text.split_at(split);
    let value: f64 = number.parse().ok()?;
    let seconds = match unit.trim() {
        "s" => value,
        "ms" => value / 1e3,
        "µs" | "us" => value / 1e6,
        "ns" => value / 1e9,
        _ => return None,
    };
    Some(Duration::from_secs_f64(seconds))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    const LIMIT: Duration = Duration::from_secs(4);

    /// The last frame being watched, and whether the watch has given up.
    fn watched(cx: &mut TestAppContext) -> (Rc<Cell<Instant>>, Rc<Cell<bool>>) {
        let last_frame = Rc::new(Cell::new(cx.executor().now()));
        let stalled = Rc::new(Cell::new(false));
        let (frame, noted) = (Rc::clone(&last_frame), Rc::clone(&stalled));
        cx.update(|cx| watch_frames(frame, LIMIT, move || noted.set(true), cx));
        (last_frame, stalled)
    }

    #[gpui::test]
    fn a_check_with_no_frames_is_given_up_on(cx: &mut TestAppContext) {
        let (_last_frame, stalled) = watched(cx);
        cx.executor().advance_clock(LIMIT / 2);
        assert!(!stalled.get(), "gave up early");
        cx.executor().advance_clock(LIMIT);
        assert!(stalled.get());
    }

    #[gpui::test]
    fn frames_arriving_keep_the_watch_quiet(cx: &mut TestAppContext) {
        let (last_frame, stalled) = watched(cx);
        for _ in 0..8 {
            cx.executor().advance_clock(LIMIT / 2);
            last_frame.set(cx.executor().now());
        }
        assert!(!stalled.get());
    }

    #[test]
    fn parses_each_duration_unit() {
        let micros = |text| parse_debug_duration(text).map(|d| d.as_secs_f64() * 1e6);
        assert_eq!(micros("1.5ms"), Some(1500.0));
        assert_eq!(micros("250µs"), Some(250.0));
        assert_eq!(micros("2s"), Some(2_000_000.0));
        assert_eq!(micros("500ns"), Some(0.5));
        assert_eq!(micros("fast"), None);
    }

    #[test]
    fn parses_what_duration_debug_prints() {
        for duration in [
            Duration::from_nanos(812),
            Duration::from_micros(4321),
            Duration::from_millis(17),
        ] {
            let parsed = parse_debug_duration(&format!("{duration:?}")).unwrap();
            assert!((parsed.as_secs_f64() - duration.as_secs_f64()).abs() < 1e-9);
        }
    }
}
