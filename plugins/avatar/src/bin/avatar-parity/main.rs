//! `avatar-parity`: replays every session log through [`Engine`] with the
//! Avatar plugin, and compares the map, the character records and the gear
//! database with what the prototype's own code reads from the same logs.
//!
//! ```text
//! cargo run --release -p mudling-plugin-avatar --bin avatar-parity -- \
//!     [--spike <mud-client-spike>] [--worlds <MUDPad Worlds folder>] [--out <folder>]
//! ```
//!
//! The report goes to `<out>/report.md`, with every difference in
//! `<out>/differences.json`; both hold game text, so `<out>` stays outside
//! the repo. See docs/parity.md for the baseline and what each kind means.
//!
//! [`Engine`]: mudling_core::session::Engine

mod analysis;
mod baseline;
mod compare;
mod log;
mod model;
mod replay;

use compare::Difference;
use directories::{BaseDirs, UserDirs};
use model::World;
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// The kinds of difference docs/parity.md explains. Any other fails the run.
const INTENDED: [&str; 12] = [
    "character.affect",
    "character.item-slot",
    "character.skill",
    "character.stat",
    "character.title",
    "character.worn",
    "gear.copies",
    "gear.identify-more-readings",
    "gear.measured-only-in-mudling",
    "gear.measured-only-in-prototype",
    "gear.slot",
    "map.description",
];
/// Named samples per kind in the report.
const SAMPLES: usize = 12;
const WORLD_FILE: &str = "avatar.outland.org.json";
const GEAR_FILE: &str = "avatar.outland.org.gear.json";

struct Paths {
    spike: PathBuf,
    worlds: PathBuf,
    out: PathBuf,
}

fn paths() -> Result<Paths, String> {
    let base = BaseDirs::new().ok_or("no home folder")?;
    let home = UserDirs::new().ok_or("no home folder")?;
    let mut paths = Paths {
        spike: home.home_dir().join("dev/vyttle/mud-client-spike"),
        worlds: base.data_dir().join("MUDPad/Worlds"),
        out: base.cache_dir().join("mudling-parity"),
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let target = match arg.as_str() {
            "--spike" => &mut paths.spike,
            "--worlds" => &mut paths.worlds,
            "--out" => &mut paths.out,
            _ => return Err(format!("unknown argument {arg}")),
        };
        *target = args.next().map(PathBuf::from).ok_or("a path is missing")?;
    }
    Ok(paths)
}

fn session_logs(spike: &Path) -> Result<Vec<PathBuf>, String> {
    let folder = spike.join("logs");
    let mut logs: Vec<PathBuf> = std::fs::read_dir(&folder)
        .map_err(|error| format!("{}: {error}", folder.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|extension| extension == "log"))
        .collect();
    logs.sort();
    Ok(logs)
}

fn read_json(path: &Path) -> Result<Value, String> {
    let bytes = std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|error| format!("{}: {error}", path.display()))
}

/// The saved world, from a copy: it's never opened where it lies.
fn saved_world(worlds: &Path, out: &Path) -> Result<World, String> {
    let copy = out.join("saved-world");
    std::fs::create_dir_all(&copy).map_err(|error| error.to_string())?;
    for file in [WORLD_FILE, GEAR_FILE] {
        std::fs::copy(worlds.join(file), copy.join(file))
            .map_err(|error| format!("{file}: {error}"))?;
    }
    Ok(model::prototype_world(
        &read_json(&copy.join(WORLD_FILE))?,
        &read_json(&copy.join(GEAR_FILE))?,
    ))
}

fn by_kind(differences: &[Difference]) -> BTreeMap<&'static str, Vec<&Difference>> {
    let mut kinds: BTreeMap<&'static str, Vec<&Difference>> = BTreeMap::new();
    for difference in differences {
        kinds.entry(difference.kind).or_default().push(difference);
    }
    kinds
}

/// How many logs there are for each day their names give.
fn logs_by_day(logs: &[PathBuf]) -> BTreeMap<String, usize> {
    let mut days = BTreeMap::new();
    for log in logs {
        let name = log.file_name().map(|name| name.to_string_lossy());
        if let Some(day) = name.as_deref().and_then(|name| name.get(8..16)) {
            *days.entry(day.to_owned()).or_default() += 1;
        }
    }
    days
}

fn counts(world: &World) -> String {
    let links: usize = world.rooms.values().map(|room| room.links.len()).sum();
    let mazes = world.rooms.values().filter(|room| room.maze).count();
    format!(
        "{} rooms, {links} links, {mazes} maze squares, {} characters, {} gear facts, {} identified items",
        world.rooms.len(),
        world.characters.len(),
        world.gear.len(),
        world.identifies.len()
    )
}

fn report(
    logs: &[PathBuf],
    prototype: &World,
    mudling: &World,
    saved: &World,
    differences: &[Difference],
    notices: &[String],
) -> String {
    let mut text = String::new();
    let mut line = |row: String| writeln!(text, "{row}").expect("a String takes writes");
    line("# Parity report".to_owned());
    line(String::new());
    line(format!("{} logs: {:?}", logs.len(), logs_by_day(logs)));
    line(String::new());
    line(format!("- Prototype's code: {}", counts(prototype)));
    line(format!("- MUDling: {}", counts(mudling)));
    line(format!("- Saved world (hand-repaired): {}", counts(saved)));
    line(format!("- Notices from the sessions: {}", notices.len()));
    for notice in notices.iter().take(SAMPLES) {
        line(format!("  - {notice}"));
    }
    line(String::new());
    line("## Differences, prototype's code / MUDling".to_owned());
    for (kind, found) in by_kind(differences) {
        line(String::new());
        line(format!("### {kind}: {}", found.len()));
        for difference in found.iter().take(SAMPLES) {
            line(format!("- {}: {}", difference.name, difference.detail));
        }
    }
    line(String::new());
    line("## Saved world against the prototype's code, by kind".to_owned());
    line(String::new());
    for (kind, found) in by_kind(&compare::compare(saved, prototype)) {
        line(format!("- {kind}: {}", found.len()));
    }
    line(String::new());
    line("## Rooms that share an identity".to_owned());
    for (name, world) in [("MUDling", mudling), ("Saved world", saved)] {
        let found = analysis::look_alikes(world);
        line(String::new());
        line(format!(
            "- {name}: {} identities, {} shared by {} rooms; {} shared only by true look-alikes; \
             {} rooms in the other {} can't be told apart",
            found.identities,
            found.shared,
            found.rooms_in_shared,
            found.true_look_alikes,
            found.maybe_copy_rooms,
            found.maybe_copies.len(),
        ));
        let largest = found.largest.iter().take(SAMPLES);
        let largest: Vec<String> = largest.map(|(n, room)| format!("{n} × {room}")).collect();
        line(format!("  - Largest: {}", largest.join("; ")));
        for copies in found.maybe_copies.iter().take(SAMPLES) {
            line(format!("  - Can't be told apart: {copies}"));
        }
    }
    line(String::new());
    line("## Maze squares".to_owned());
    for (name, world) in [("MUDling", mudling), ("Saved world", saved)] {
        line(format!("- {name}: {:?}", analysis::maze_squares(world)));
    }
    line(String::new());
    line("## Rooms with no description".to_owned());
    for (name, world) in [("MUDling", mudling), ("Saved world", saved)] {
        let rooms = analysis::rooms_without_a_description(world);
        line(format!("- {name}: {} {rooms:?}", rooms.len()));
    }
    text
}

fn run() -> Result<(), String> {
    let paths = paths()?;
    std::fs::create_dir_all(&paths.out).map_err(|error| error.to_string())?;
    let logs = session_logs(&paths.spike)?;
    let baseline = baseline::ensure(&paths.spike, &logs, &paths.out)?;
    let saved_baseline = read_json(&baseline)?;
    let prototype = model::prototype_world(&saved_baseline, &saved_baseline["gear"]);
    let saved = saved_world(&paths.worlds, &paths.out)?;

    let data = paths.out.join("replay-data");
    if data.exists() {
        std::fs::remove_dir_all(&data).map_err(|error| error.to_string())?;
    }
    let replayed = replay::run(&logs, &data)?;
    let differences = compare::compare(&prototype, &replayed.world);

    let text = report(
        &logs,
        &prototype,
        &replayed.world,
        &saved,
        &differences,
        &replayed.notices,
    );
    let report_path = paths.out.join("report.md");
    std::fs::write(&report_path, &text).map_err(|error| error.to_string())?;
    let json = serde_json::to_string_pretty(&differences).map_err(|error| error.to_string())?;
    std::fs::write(paths.out.join("differences.json"), json).map_err(|error| error.to_string())?;
    println!(
        "{} differences; report at {}",
        differences.len(),
        report_path.display()
    );
    let kinds = by_kind(&differences);
    let unexplained: Vec<&str> = kinds
        .keys()
        .copied()
        .filter(|kind| !INTENDED.contains(kind))
        .collect();
    if !unexplained.is_empty() {
        return Err(format!("not in docs/parity.md: {unexplained:?}"));
    }
    if !replayed.notices.is_empty() {
        return Err(format!("the sessions noted {:?}", replayed.notices));
    }
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(2)
        }
    }
}
