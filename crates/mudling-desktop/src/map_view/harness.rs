//! The map on its own, on a synthetic map, before the shell mounts it.
//!
//! ```text
//! mudling-desktop --map-harness [--rooms <n>] [--light | --dark] [--dark-walk]
//!                 [--duplicates] [--measure [--seconds <n>] [--report <path>]]
//! ```
//!
//! `--duplicates` draws rooms that read alike and opens Review on them.
//!
//! `--measure` relaunches with gpui's frame timing on, like the chat's
//! `--measure`, and drives pan then zoom with every room in view. It writes
//! a JSON report, prints each phase, and judges pan and zoom against the
//! 8 ms budget. Maps here
//! are made up; no real map data belongs in this file.
//!
//! gpui's frame time includes waiting for a free Metal drawable. On a busy
//! machine one late frame can lock the run on to that wait, every frame near
//! 8 or 16 ms with no more work done, often for the rest of the run.
//! `--rooms 25` is the control: it locks on the same way, so only a phase
//! that stays slow beside a fast control is the map.
//!
//! Built with `--features shots`, `--shot <path.png>` saves a picture of the
//! window and quits, and `--shot-fit <path.png>` frames the whole map first.

use super::{MapLayout, MapView, Selection, Walk};
use crate::assets;
use crate::measure;
use crate::theme::Theme;
use gpui::{
    ActivityGuard, App, AppContext, Bounds, Context, Entity, Render, Size, Subscription,
    TitlebarOptions, Window, WindowAppearance, WindowBounds, WindowOptions, div, prelude::*, px,
};
use mudling_core::map::{
    Area, AreaName, Building, Derived, DirMap, DirSet, Direction, ExitState, GridPoint, MapChange,
    MapDoor, MapRoom, RoomId, Whereabouts, feature,
};
use mudling_core::stats::Summary;
use mudling_core::store::{Diff, MapState, Revision, StoreState, Versioned};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const ZONE: &str = "The Synthetic Reach";
const NEXT_ZONE: &str = "The Made-up Marches";
const WINDOW: Size<gpui::Pixels> = Size {
    width: px(1400.),
    height: px(1000.),
};
const INSET: Size<gpui::Pixels> = Size {
    width: px(376.),
    height: px(202.),
};
const FRAME_BUDGET_US: f64 = 8_000.0;
const BUDGETED_PHASES: [&str; 2] = ["pan", "zoom"];
const SETTLE: Duration = Duration::from_secs(1);
/// Backs off a little from a tight fit, so panning keeps every room in view.
const FIT_MARGIN: f32 = 0.92;
/// Pixels a frame: every frame redraws the whole map, however far it moves.
const PAN_STEP: f32 = 1.;
/// Frames in each direction while panning, and each way while zooming.
const SWING_FRAMES: u32 = 30;
const ZOOM_STEP: f32 = 1.002;
/// The smallest grid the sample map's features fit on.
const MIN_SIDE: i64 = 5;
/// Ids clear of the grid's.
const BUILDING_IDS: RoomId = 1 << 32;
/// With `--duplicates`, how many squares apart rooms read the same.
const DUPLICATES_REPEAT: i64 = 7;

// MARK: Synthetic maps

/// A room on the ground floor of the synthetic area, with nothing known.
pub fn room(id: RoomId, x: i64, y: i64) -> MapRoom {
    MapRoom {
        id,
        name: format!("Square {x}, {y}"),
        zone: ZONE.to_owned(),
        environment: String::new(),
        indoor: false,
        description: String::new(),
        position: GridPoint::new(x, y, 0),
        building: None,
        exits: DirMap::default(),
        links: DirMap::default(),
        doorways: DirSet::default(),
        tangled: false,
        maze: false,
        notes: String::new(),
        features: BTreeSet::new(),
        sightings: BTreeMap::new(),
    }
}

/// A map store holding `rooms`, with the player in the first.
pub fn store_of(rooms: Vec<MapRoom>, buildings: Vec<Building>) -> MapState {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for room in &rooms {
        *counts.entry(room.zone.clone()).or_default() += 1;
    }
    let areas = counts
        .into_iter()
        .map(|(zone, rooms)| Area {
            name: AreaName {
                name: zone.clone(),
                levels: None,
            },
            zone,
            rooms,
        })
        .collect();
    let mut map = MapState::default();
    map.apply(&MapChange {
        revision: 1,
        whereabouts: Some(Whereabouts {
            room: rooms.first().map(|room| room.id),
            ..Whereabouts::default()
        }),
        rooms: rooms.into_iter().map(Arc::new).collect(),
        derived: Some(Arc::new(Derived {
            revision: 1,
            areas,
            buildings,
        })),
        ..MapChange::default()
    });
    map
}

/// The store a shell would hand the view for this map.
pub fn versioned(map: MapState) -> Versioned<MapState> {
    let mut store = Versioned::default();
    store.apply_diff(&Diff {
        revision: Revision(1),
        change: MapChange {
            revision: 1,
            rooms: map.rooms().cloned().collect(),
            whereabouts: Some(map.whereabouts()),
            derived: map.derived().cloned(),
            ..MapChange::default()
        },
    });
    store
}

fn link(rooms: &mut [MapRoom], index: usize, direction: Direction, other: usize) {
    let (to, from) = (rooms[other].id, rooms[index].id);
    rooms[index].links.insert(direction, to);
    rooms[other].links.insert(direction.opposite(), from);
    for (room, way) in [(index, direction), (other, direction.opposite())] {
        let exit = ExitState {
            state: String::new(),
            closed: false,
        };
        rooms[room].exits.insert(way, exit);
    }
}

fn grid_rooms(side: i64) -> Vec<MapRoom> {
    let id = |x: i64, y: i64| y * side + x;
    let mut rooms: Vec<MapRoom> = (0..side * side)
        .map(|n| room(n, n % side, n / side))
        .collect();
    for y in 0..side {
        for x in 0..side {
            let here = id(x, y) as usize;
            if x + 1 < side {
                link(&mut rooms, here, Direction::East, id(x + 1, y) as usize);
            }
            if y + 1 < side {
                link(&mut rooms, here, Direction::North, id(x, y + 1) as usize);
            }
        }
    }
    rooms
}

/// A `side` by `side` grid with a note on every room. Made up for measuring.
pub fn noted_map(side: i64) -> MapState {
    let mut rooms = grid_rooms(side);
    for room in &mut rooms {
        room.notes = format!("Check back here after a reboot: {}.", room.name);
    }
    store_of(rooms, Vec::new())
}

/// A `side` by `side` grid of linked rooms.
#[cfg(test)]
pub fn grid_map(side: i64) -> MapState {
    store_of(grid_rooms(side), Vec::new())
}

/// A grid on a game with no room numbers, where every room reads the same as
/// the ones `repeat` squares away: rooms the map marks as likely duplicates.
pub fn duplicated_map(side: i64, repeat: i64) -> MapState {
    const FIRST_ID: RoomId = 1 << 40;
    let mut rooms = grid_rooms(side);
    for room in &mut rooms {
        let (x, y) = (room.position.x, room.position.y);
        room.id += FIRST_ID;
        room.name = format!("Square {}, {}", x % repeat, y % repeat);
        let links: Vec<_> = room.links.iter().map(|(way, to)| (way, *to)).collect();
        for (way, to) in links {
            room.links.insert(way, to + FIRST_ID);
        }
    }
    store_of(rooms, Vec::new())
}

/// A grid with one of everything the map draws: aggressive mobs, mazes,
/// floors, a building that fits the outdoors, one that's a house at its
/// door, and a way out of the area.
pub fn sample_map(side: i64, dark_walk: bool) -> MapState {
    let mut rooms = grid_rooms(side);
    let id = |x: i64, y: i64| (y * side + x) as usize;
    for room in &mut rooms {
        let (x, y) = (room.position.x, room.position.y);
        if (x * 7 + y * 3) % 19 == 0 {
            room.features.insert(feature::AGGRESSIVE_MOB.to_owned());
        }
        if (x * 3 + y * 5) % 11 == 0 {
            room.features.insert(feature::SHOP.to_owned());
            room.features.insert(feature::GEAR_MOB.to_owned());
            room.notes = "A note".to_owned();
        }
        room.maze = x % 13 == 6 && y % 13 == 6;
    }

    let stairs = id(side / 2, side / 2);
    let mut upstairs = room(BUILDING_IDS, side / 2, side / 2);
    upstairs.position.z = 1;
    rooms.push(upstairs);
    let top = rooms.len() - 1;
    link(&mut rooms, stairs, Direction::Up, top);
    let mut landing = room(BUILDING_IDS + 1, side / 2 + 1, side / 2);
    landing.position.z = 1;
    rooms.push(landing);
    let last = rooms.len() - 1;
    link(&mut rooms, top, Direction::East, last);

    let keep = BUILDING_IDS + 10;
    let mut hall = room(keep, 0, 0);
    hall.building = Some(keep);
    let mut tower = room(keep + 1, 1, 0);
    tower.building = Some(keep);
    rooms.extend([hall, tower]);
    let (hall, tower) = (rooms.len() - 2, rooms.len() - 1);
    link(&mut rooms, hall, Direction::East, tower);
    let door = id(0, side / 3);
    link(&mut rooms, door, Direction::West, hall);

    let shop = BUILDING_IDS + 20;
    let mut front = room(shop, 0, 0);
    front.building = Some(shop);
    let mut back = room(shop + 1, 1, 0);
    back.building = Some(shop);
    rooms.extend([front, back]);
    let (front, back) = (rooms.len() - 2, rooms.len() - 1);
    link(&mut rooms, front, Direction::East, back);
    let street = id(side - 1, side / 3);
    link(&mut rooms, street, Direction::East, front);

    let mut road = room(BUILDING_IDS + 30, side / 2, side);
    road.zone = NEXT_ZONE.to_owned();
    rooms.push(road);
    let road = rooms.len() - 1;
    link(&mut rooms, id(side / 2, side - 1), Direction::North, road);

    rooms.swap(0, id(side / 2, side / 2 - 2));
    let buildings = vec![
        Building {
            id: keep,
            doors: vec![MapDoor {
                outside: id(0, side / 3) as RoomId,
                direction: Direction::West,
                inside: keep,
            }],
            footprint: None,
        },
        Building {
            id: shop,
            doors: vec![MapDoor {
                outside: id(side - 1, side / 3) as RoomId,
                direction: Direction::East,
                inside: shop,
            }],
            footprint: Some(GridPoint::new(side, side / 3, 0)),
        },
    ];
    let mut map = store_of(rooms, buildings);
    if dark_walk {
        let you = map.whereabouts().room;
        map.apply(&MapChange {
            revision: 2,
            whereabouts: Some(Whereabouts {
                room: you,
                dark: true,
                dark_position: Some(GridPoint::new(-2, side / 2, 0)),
                ..Whereabouts::default()
            }),
            ..MapChange::default()
        });
    }
    map
}

// MARK: Running it

struct Options {
    side: i64,
    /// `None` follows the OS.
    appearance: Option<WindowAppearance>,
    dark_walk: bool,
    /// Rooms that read alike, so the map marks them, with Review open.
    duplicates: bool,
    measure: bool,
    measured_run: bool,
    seconds: u64,
    report: PathBuf,
    #[cfg(feature = "shots")]
    shot: Option<Shot>,
}

fn parse_options(args: &[String]) -> Result<Options, String> {
    let mut options = Options {
        side: 45,
        appearance: None,
        dark_walk: false,
        duplicates: false,
        measure: false,
        measured_run: false,
        seconds: 5,
        report: PathBuf::from("reports/map-frame-report.json"),
        #[cfg(feature = "shots")]
        shot: None,
    };
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or(format!("{arg} needs a value"));
        match arg.as_str() {
            "--map-harness" => {}
            "--rooms" => {
                let rooms: f64 = value()?.parse().map_err(|_| "--rooms needs a number")?;
                options.side = (rooms.sqrt().ceil() as i64).max(MIN_SIDE);
            }
            "--light" => options.appearance = Some(WindowAppearance::Light),
            "--dark" => options.appearance = Some(WindowAppearance::Dark),
            "--dark-walk" => options.dark_walk = true,
            "--duplicates" => options.duplicates = true,
            "--measure" => options.measure = true,
            "--measured-run" => options.measured_run = true,
            "--seconds" => {
                options.seconds = value()?.parse().map_err(|_| "--seconds needs a number")?
            }
            "--report" => options.report = PathBuf::from(value()?),
            #[cfg(feature = "shots")]
            "--shot" | "--shot-fit" => {
                let fit = arg == "--shot-fit";
                let path = PathBuf::from(value()?);
                options.shot = Some(Shot { path, fit });
            }
            other => return Err(format!("unknown option {other}")),
        }
    }
    Ok(options)
}

/// Runs the harness, in the background like `main`'s checks; the rest of
/// `main` is the chat.
pub fn run(args: &[String]) -> ExitCode {
    let options = match parse_options(args) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };
    if options.measure {
        let child: Vec<String> = args.iter().filter(|a| *a != "--measure").cloned().collect();
        return run_measured(&child, &options);
    }
    let started = Instant::now();
    let map = if options.duplicates {
        duplicated_map(options.side, DUPLICATES_REPEAT)
    } else {
        sample_map(options.side, options.dark_walk)
    };
    if options.measured_run {
        let built_ms = started.elapsed().as_secs_f64() * 1e3;
        measure::mark(&format!("info synthetic_map_ms={built_ms:.1}"));
    }
    // Like a check, the harness never comes forward: the player may be
    // working. See `main`.
    let application = gpui_platform::application().with_assets(assets::Assets);
    #[cfg(target_os = "macos")]
    let application = application.with_windowing(gpui::WindowingRequest::Headless);
    application.run(move |cx: &mut App| {
        assets::load_fonts(cx);
        cx.on_window_closed(|cx, _| cx.quit()).detach();
        let bounds = Bounds::centered(None, WINDOW, cx);
        let window_options = WindowOptions {
            titlebar: Some(TitlebarOptions {
                title: Some("MUDling map harness".into()),
                ..TitlebarOptions::default()
            }),
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            focus: false,
            // macOS shows it only once `macos::show_unseen` has hidden it.
            show: !cfg!(target_os = "macos"),
            // A measured run draws every frame even if another app has focus.
            inactive_frame_interval: if options.measured_run {
                None
            } else {
                WindowOptions::default().inactive_frame_interval
            },
            ..WindowOptions::default()
        };
        let (appearance, measured, seconds, review) = (
            options.appearance,
            options.measured_run,
            options.seconds,
            options.duplicates,
        );
        #[cfg(feature = "shots")]
        let shot = options.shot;
        let opened = cx.open_window(window_options, |window, cx| {
            let appearance = appearance.unwrap_or(window.appearance());
            cx.set_global(Theme::for_appearance(appearance));
            cx.new(|cx| Harness::new(map, measured, cx))
        });
        let window = match opened {
            Ok(window) => window,
            Err(error) => {
                eprintln!("couldn't open the window: {error}");
                cx.quit();
                return;
            }
        };
        #[cfg(target_os = "macos")]
        crate::macos::show_unseen(cx);
        measure::fail_if_frames_stop(window.into(), cx);
        let Ok(map) = window.update(cx, |harness, _, _| harness.map.clone()) else {
            return;
        };
        if measured {
            start_driver(window.into(), map.clone(), Duration::from_secs(seconds), cx);
        } else if review {
            window
                .update(cx, |_, window, cx| {
                    map.update(cx, |map, cx| map.review_sample(window, cx));
                })
                .ok();
        }
        #[cfg(feature = "shots")]
        if let Some(shot) = shot {
            window
                .update(cx, |_, window, _| {
                    window.on_next_frame(move |window, cx| shot.take(map, 0, window, cx));
                })
                .ok();
        }
    });
    ExitCode::SUCCESS
}

/// A picture of the window, drawn offscreen, so no screen access is needed.
#[cfg(feature = "shots")]
struct Shot {
    path: PathBuf,
    /// Fit the whole sheet, instead of the player at zoom 1.
    fit: bool,
}

#[cfg(feature = "shots")]
impl Shot {
    /// Frames to let the sheet and the fit reach the screen.
    const SETTLE_FRAMES: u32 = 10;

    fn take(self, map: Entity<MapView>, frames: u32, window: &mut Window, cx: &mut App) {
        let ready = map.read(cx).model().sheet().room_count() > 0;
        if ready && frames == 0 && self.fit {
            map.update(cx, |view, cx| {
                let size = view.view_size();
                view.model.fit((size.0 * FIT_MARGIN, size.1 * FIT_MARGIN));
                cx.notify();
            });
        }
        if !ready || frames < Self::SETTLE_FRAMES {
            let frames = if ready { frames + 1 } else { 0 };
            map.update(cx, |_, cx| cx.notify());
            window.on_next_frame(move |window, cx| self.take(map, frames, window, cx));
            return;
        }
        let saved = window.render_to_image().and_then(|image| {
            image.save(&self.path)?;
            Ok(())
        });
        if let Err(error) = saved {
            eprintln!("couldn't save {}: {error}", self.path.display());
        }
        cx.quit();
    }
}

struct Harness {
    map: Entity<MapView>,
    inset: Option<Entity<MapView>>,
    _picks: Subscription,
}

impl Harness {
    fn new(map: MapState, measured: bool, cx: &mut Context<Harness>) -> Harness {
        let store = versioned(map);
        let panel = store.clone();
        let view = cx.new(|cx| MapView::new(panel, MapLayout::Panel, cx));
        let inset = (!measured).then(|| cx.new(|cx| MapView::new(store, MapLayout::Inset, cx)));
        let picks = cx.subscribe(&view, |_, _, _: &Selection, cx| cx.notify());
        Harness {
            map: view,
            inset,
            _picks: picks,
        }
    }

    fn picked_text(&self, cx: &App) -> String {
        let view = self.map.read(cx);
        let Some(selection) = view.selection() else {
            return "Click a room.".to_owned();
        };
        let name = view.model().room_name(selection.room).unwrap_or("A room");
        match &selection.walk {
            Walk::Unknown => format!("{name}: working out the way"),
            Walk::NoPath => format!("{name}: no explored path"),
            Walk::Steps(steps) => {
                let words: Vec<&str> = steps
                    .iter()
                    .take(12)
                    .map(|step| &step.direction.name()[..1])
                    .collect();
                format!("{name}: {} moves, {}", steps.len(), words.join(" "))
            }
        }
    }
}

impl Render for Harness {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Harness>) -> impl IntoElement {
        let theme = cx.global::<Theme>();
        let (background, heading, muted) = (theme.background, theme.map.heading, theme.map.muted);
        let picked = self.picked_text(cx);
        div()
            .size_full()
            .flex()
            .gap_4()
            .p_4()
            .bg(background)
            .text_color(heading)
            .child(div().flex_1().min_w_0().child(self.map.clone()))
            .when_some(self.inset.clone(), |root, inset| {
                root.child(
                    div()
                        .w(INSET.width)
                        .flex()
                        .flex_col()
                        .gap_4()
                        .child(div().h(INSET.height).child(inset))
                        .child(div().text_size(px(13.)).text_color(muted).child(picked)),
                )
            })
    }
}

// MARK: Measuring

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Waiting,
    Settling,
    Pan,
    Zoom,
    /// The player walks a room a frame, through store diffs.
    Moves,
}

struct Driver {
    map: Entity<MapView>,
    seconds: Duration,
    phase: Phase,
    entered: Instant,
    frames: u32,
    fewest_in_view: usize,
    _no_app_nap: ActivityGuard,
}

impl Driver {
    fn on_frame(mut self, window: &mut Window, cx: &mut App) {
        let Some(next) = self.step(cx) else {
            measure::mark(&format!("info rooms_in_view_min={}", self.fewest_in_view));
            measure::mark("phase end");
            cx.quit();
            return;
        };
        if next != self.phase {
            self.phase = next;
            self.entered = Instant::now();
            self.frames = 0;
            match next {
                Phase::Pan => measure::mark("phase pan"),
                Phase::Zoom => measure::mark("phase zoom"),
                Phase::Moves => measure::mark("phase moves"),
                Phase::Waiting | Phase::Settling => {}
            }
        }
        window.on_next_frame(move |window, cx| self.on_frame(window, cx));
    }

    /// Runs one frame of the phase; returns the phase to be in, `None` at
    /// the end.
    fn step(&mut self, cx: &mut App) -> Option<Phase> {
        let elapsed = self.entered.elapsed();
        self.frames += 1;
        let swing = if (self.frames / SWING_FRAMES).is_multiple_of(2) {
            1.
        } else {
            -1.
        };
        let in_view = self.map.update(cx, |view, cx| {
            let size = view.view_size();
            let rooms = view.model.sheet().room_count();
            match self.phase {
                Phase::Waiting if rooms > 0 && size.0 > 0. => {
                    view.model.fit((size.0 * FIT_MARGIN, size.1 * FIT_MARGIN));
                }
                Phase::Pan => view.model.pan((PAN_STEP * swing, PAN_STEP * swing)),
                Phase::Zoom => {
                    let middle = (size.0 / 2., size.1 / 2.);
                    view.model.zoom(ZOOM_STEP.powf(swing), middle, size);
                }
                Phase::Moves => {
                    let frame = u64::from(self.frames);
                    let diff = Diff {
                        revision: Revision(1 + frame),
                        change: MapChange {
                            revision: 1 + frame,
                            whereabouts: Some(Whereabouts {
                                room: Some(
                                    view.model.first_room() + frame as RoomId % rooms as RoomId,
                                ),
                                ..Whereabouts::default()
                            }),
                            ..MapChange::default()
                        },
                    };
                    view.apply(&diff, cx);
                }
                Phase::Waiting | Phase::Settling => {}
            }
            cx.notify();
            let region = view.model.camera().region(size);
            (rooms, view.model.sheet().tiles_in(region).count())
        });
        match self.phase {
            Phase::Waiting if in_view.0 > 0 => Some(Phase::Settling),
            Phase::Waiting => Some(Phase::Waiting),
            Phase::Settling if elapsed >= SETTLE => Some(Phase::Pan),
            Phase::Settling => Some(Phase::Settling),
            Phase::Pan | Phase::Zoom | Phase::Moves => {
                self.fewest_in_view = self.fewest_in_view.min(in_view.1);
                if elapsed < self.seconds {
                    return Some(self.phase);
                }
                match self.phase {
                    Phase::Pan => Some(Phase::Zoom),
                    Phase::Zoom => Some(Phase::Moves),
                    _ => None,
                }
            }
        }
    }
}

fn start_driver(
    window: gpui::AnyWindowHandle,
    map: Entity<MapView>,
    seconds: Duration,
    cx: &mut App,
) {
    let driver = Driver {
        map,
        seconds,
        phase: Phase::Waiting,
        entered: Instant::now(),
        frames: 0,
        fewest_in_view: usize::MAX,
        _no_app_nap: cx
            .background_executor()
            .prevent_app_nap("MUDling measured map"),
    };
    window
        .update(cx, |_, window, _| {
            window.on_next_frame(move |window, cx| driver.on_frame(window, cx))
        })
        .expect("the measured window is open");
}

#[derive(Serialize)]
struct MapFrameReport {
    os: &'static str,
    arch: &'static str,
    optimized: bool,
    /// Other work on the machine skews frame times; this says how busy it was.
    load_average_1m_start: Option<f64>,
    load_average_1m_end: Option<f64>,
    frame_budget_us: f64,
    within_budget: bool,
    phases: BTreeMap<String, Summary>,
    info: BTreeMap<String, f64>,
}

/// Runs the harness as a measured child and writes the report.
fn run_measured(child_args: &[String], options: &Options) -> ExitCode {
    let exe = std::env::current_exe().expect("the running binary has a path");
    let load_average_1m_start = measure::load_average();
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

    let mut phase = String::new();
    let mut frames: BTreeMap<String, Vec<Duration>> = BTreeMap::new();
    let mut info = BTreeMap::new();
    let stderr = BufReader::new(child.stderr.take().expect("stderr is piped"));
    for line in stderr.lines().map_while(Result::ok) {
        if let Some(duration) = measure::frame_duration(&line) {
            if !phase.is_empty() {
                frames.entry(phase.clone()).or_default().push(duration);
            }
        } else if let Some(marker) = measure::marker(&line) {
            if let Some(name) = marker.strip_prefix("phase ") {
                phase = name.to_owned();
            } else if let Some((key, value)) = marker
                .strip_prefix("info ")
                .and_then(|pair| pair.split_once('='))
                && let Ok(value) = value.parse()
            {
                info.insert(key.to_owned(), value);
            }
        } else {
            eprintln!("{line}");
        }
    }
    let status = child.wait();
    if phase != "end" {
        eprintln!("the measured run ended early ({status:?}) during {phase:?}");
        return ExitCode::FAILURE;
    }

    let phases: BTreeMap<String, Summary> = frames
        .iter()
        .filter_map(|(name, samples)| Some((name.clone(), Summary::of(samples)?)))
        .collect();
    let report = MapFrameReport {
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        optimized: !cfg!(debug_assertions),
        load_average_1m_start,
        load_average_1m_end: measure::load_average(),
        frame_budget_us: FRAME_BUDGET_US,
        within_budget: within_budget(&phases),
        phases,
        info,
    };
    print_report(&report);
    let json = serde_json::to_string_pretty(&report).expect("the report serializes");
    if let Some(folder) = options.report.parent() {
        std::fs::create_dir_all(folder).ok();
    }
    if let Err(error) = std::fs::write(&options.report, json) {
        eprintln!("couldn't write {}: {error}", options.report.display());
        return ExitCode::FAILURE;
    }
    println!("report: {}", options.report.display());
    ExitCode::SUCCESS
}

/// Whether pan and zoom, the phases the budget names, both met it. Moves and
/// the frame drawn as the run ends are reported but not judged.
fn within_budget(phases: &BTreeMap<String, Summary>) -> bool {
    BUDGETED_PHASES.iter().all(|name| {
        phases
            .get(*name)
            .is_some_and(|phase| phase.p99_us <= FRAME_BUDGET_US)
    })
}

fn print_report(report: &MapFrameReport) {
    println!(
        "load average (1 min): {:?} at start, {:?} at end",
        report.load_average_1m_start, report.load_average_1m_end
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
    println!(
        "pan and zoom frame p99 ≤ 8 ms: {}",
        if report.within_budget { "ok" } else { "MISSED" }
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn phases(p99s: &[(&str, f64)]) -> BTreeMap<String, Summary> {
        let summary = |p99_ms: f64| Summary {
            samples: 480,
            mean_us: 2_000.,
            p50_us: 2_000.,
            p99_us: p99_ms * 1e3,
            max_us: p99_ms * 1e3,
        };
        (p99s.iter())
            .map(|(name, p99_ms)| (name.to_string(), summary(*p99_ms)))
            .collect()
    }

    #[test]
    fn slow_moves_and_end_frames_dont_miss_the_budget() {
        let run = phases(&[("pan", 4.), ("zoom", 3.6), ("moves", 11.), ("end", 9.)]);
        assert!(within_budget(&run));
    }

    #[test]
    fn a_slow_pan_or_zoom_misses_the_budget() {
        let slow_pan = phases(&[("pan", 8.5), ("zoom", 3.6)]);
        let slow_zoom = phases(&[("pan", 4.), ("zoom", 8.5)]);
        assert!(!within_budget(&slow_pan));
        assert!(!within_budget(&slow_zoom));
    }

    #[test]
    fn a_run_missing_pan_or_zoom_misses_the_budget() {
        assert!(!within_budget(&phases(&[("zoom", 3.6), ("moves", 4.)])));
    }
}
