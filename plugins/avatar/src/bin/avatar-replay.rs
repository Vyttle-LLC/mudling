//! `avatar-replay <session.log>…`: replays the prototype's session logs
//! through the line pipeline, the plugin host and the Luau hooks with the
//! Avatar plugin on, and reports per log how many reads of each table
//! completed, and which it started but didn't finish.
//!
//! Logs hold other players' chat: they're read at runtime only, and the
//! report quotes only the first line of each unfinished table.

use mudling_core::plugin::PluginEvent;
use mudling_core::plugin::luau::{HookOutput, StoreWrite};
use mudling_core::replay;
use mudling_plugin_avatar::Session;
use std::fmt::Write;
use std::path::Path;
use std::process::ExitCode;
use std::time::Instant;

const READERS: [Reader; 7] = [
    Reader::Score,
    Reader::Practice,
    Reader::Slist,
    Reader::Affects,
    Reader::Equipment,
    Reader::Identify,
    Reader::Examine,
];
/// Unfinished reads quoted per log.
const QUOTED: usize = 3;
const QUOTE_CHARS: usize = 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reader {
    Score,
    Practice,
    Slist,
    Affects,
    Equipment,
    Identify,
    Examine,
}

impl Reader {
    fn name(self) -> &'static str {
        match self {
            Reader::Score => "score",
            Reader::Practice => "practice",
            Reader::Slist => "slist",
            Reader::Affects => "aff",
            Reader::Equipment => "eq",
            Reader::Identify => "identify",
            Reader::Examine => "examine",
        }
    }

    /// The reader an event opens a read of. "You are not affected by any"
    /// is a whole read on its own line, so it opens none.
    fn started_by(event: &str) -> Option<Reader> {
        Some(match event {
            "character.score.start" => Reader::Score,
            "character.practice.start" => Reader::Practice,
            "character.slist.start" => Reader::Slist,
            "character.affects.start" => Reader::Affects,
            "gear.equipment.start" => Reader::Equipment,
            "gear.identify.named" | "gear.identify.object" => Reader::Identify,
            "gear.examine.item" => Reader::Examine,
            _ => return None,
        })
    }

    /// The reader whose finished read `write` ends.
    fn finished_by(write: &StoreWrite) -> Option<Reader> {
        Some(match (write.store.as_str(), write.key.as_str()) {
            ("character", "score.read") => Reader::Score,
            ("skills", "practice.read") => Reader::Practice,
            ("skills", "slist.read") => Reader::Slist,
            ("affects", "count") => Reader::Affects,
            ("equipment", "count") => Reader::Equipment,
            ("identify", "read") => Reader::Identify,
            ("gear", key) if key.starts_with("slot.") => Reader::Examine,
            _ => return None,
        })
    }

    /// A start line that continues the read open rather than starting
    /// another: practice's sessions line then its header; a shop's name
    /// line then its object.
    fn continues(self, event: &str) -> bool {
        matches!(
            (self, event),
            (Reader::Practice, "character.practice.start")
                | (Reader::Identify, "gear.identify.object")
        )
    }
}

/// A read begun and not yet finished.
struct Open {
    reader: Reader,
    first_line: String,
    /// A row was read: until then, the start may have been another line.
    rows: bool,
    paged: bool,
}

#[derive(Default)]
struct Tally {
    lines: usize,
    completed: [usize; READERS.len()],
    unfinished: [usize; READERS.len()],
    quoted: Vec<String>,
    hook_failures: usize,
    pager_prompts: usize,
}

impl Tally {
    fn index(reader: Reader) -> usize {
        READERS.iter().position(|known| *known == reader).unwrap()
    }

    fn unfinished(&mut self, open: Open, why: &str) {
        // An examine of something not worn has no slot to find.
        if !open.rows || open.reader == Reader::Examine {
            return;
        }
        self.unfinished[Tally::index(open.reader)] += 1;
        if self.quoted.len() < QUOTED {
            let line: String = open.first_line.chars().take(QUOTE_CHARS).collect();
            self.quoted
                .push(format!("{} ({why}): {line}", open.reader.name()));
        }
    }
}

struct Replay {
    tally: Tally,
    open: Option<Open>,
}

impl Replay {
    /// Sees the read an event's outputs finished, then the read it starts:
    /// a shop's identify line finishes the identify before it.
    fn observe(&mut self, event: &PluginEvent, outputs: &[HookOutput]) {
        let name = &*event.name;
        for output in outputs {
            match output {
                HookOutput::StoreWrite(write) => {
                    let Some(reader) = Reader::finished_by(write) else {
                        continue;
                    };
                    let was_open = self.open.as_ref().is_some_and(|open| open.reader == reader);
                    // Other reads note slots too; only an open examine ends by one.
                    if reader != Reader::Examine || was_open {
                        self.tally.completed[Tally::index(reader)] += 1;
                    }
                    if was_open {
                        self.open = None;
                    }
                }
                HookOutput::HookFailed(_) | HookOutput::PluginDisabled(_) => {
                    self.tally.hook_failures += 1
                }
                HookOutput::SendCommand(_) => {}
            }
        }
        if let Some(reader) = Reader::started_by(name) {
            let continuing = self
                .open
                .as_ref()
                .is_some_and(|open| open.reader == reader && reader.continues(name));
            if !continuing {
                if let Some(open) = self.open.take() {
                    self.tally
                        .unfinished(open, &format!("interrupted by {}", reader.name()));
                }
                self.open = Some(Open {
                    reader,
                    first_line: event.line.plain().to_owned(),
                    rows: false,
                    paged: false,
                });
            }
        }
        match name {
            "pager" => {
                if let Some(open) = &mut self.open {
                    open.paged = true;
                }
            }
            name if Reader::started_by(name).is_none()
                && (name.ends_with(".row") || name.starts_with("gear.identify.")) =>
            {
                if let Some(open) = &mut self.open {
                    open.rows = true;
                    open.paged = false;
                }
            }
            _ => {}
        }
        if name == "prompt"
            && let Some(open) = self.open.take()
        {
            let why = if open.paged {
                "pager not answered"
            } else {
                "not finished by the prompt"
            };
            self.tally.unfinished(open, why);
        }
    }
}

fn replay(log: &str) -> Tally {
    let mut session = Session::new();
    let mut replay = Replay {
        tally: Tally::default(),
        open: None,
    };
    for line in replay::server_lines(log) {
        replay.tally.lines += 1;
        session.read_line(line, &mut |event, outputs| replay.observe(event, outputs));
    }
    if let Some(open) = replay.open.take() {
        replay.tally.unfinished(open, "log ended");
    }
    replay.tally.pager_prompts = session.pager_prompts;
    replay.tally
}

fn main() -> ExitCode {
    let logs: Vec<String> = std::env::args().skip(1).collect();
    if logs.is_empty() {
        eprintln!("usage: avatar-replay <session.log>…");
        return ExitCode::from(2);
    }
    let started = Instant::now();
    let mut total = Tally::default();
    let mut header = format!("{:<28} {:>7}", "log", "lines");
    for reader in READERS {
        write!(header, " {:>8}", reader.name()).unwrap();
    }
    println!("{header} {:>10} {:>6}", "unfinished", "failed");
    for path in &logs {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) => {
                eprintln!("couldn't read {path}: {error}");
                return ExitCode::from(2);
            }
        };
        let tally = replay(&String::from_utf8_lossy(&bytes));
        let name = Path::new(path)
            .file_name()
            .map_or(path.clone(), |name| name.to_string_lossy().into_owned());
        let mut row = format!("{name:<28} {:>7}", tally.lines);
        for count in tally.completed {
            write!(row, " {count:>8}").unwrap();
        }
        println!(
            "{row} {:>10} {:>6}",
            tally.unfinished.iter().sum::<usize>(),
            tally.hook_failures
        );
        for quoted in &tally.quoted {
            println!("    unfinished {quoted}");
        }
        total.lines += tally.lines;
        total.hook_failures += tally.hook_failures;
        total.pager_prompts += tally.pager_prompts;
        for index in 0..READERS.len() {
            total.completed[index] += tally.completed[index];
            total.unfinished[index] += tally.unfinished[index];
        }
    }
    let mut row = format!("{:<28} {:>7}", "total", total.lines);
    for count in total.completed {
        write!(row, " {count:>8}").unwrap();
    }
    println!(
        "{row} {:>10} {:>6}",
        total.unfinished.iter().sum::<usize>(),
        total.hook_failures
    );
    let unfinished: Vec<String> = READERS
        .iter()
        .zip(total.unfinished)
        .filter(|(_, count)| *count > 0)
        .map(|(reader, count)| format!("{} {count}", reader.name()))
        .collect();
    println!(
        "unfinished by table: {}",
        if unfinished.is_empty() {
            "none".to_owned()
        } else {
            unfinished.join(", ")
        }
    );
    println!(
        "pager prompts recognized: {}; {} logs in {:.1} s",
        total.pager_prompts,
        logs.len(),
        started.elapsed().as_secs_f64()
    );
    ExitCode::SUCCESS
}
