//! Quiet reads: the plugins' background commands, sent when the game can spare
//! them, with their answers kept out of the chat.
//!
//! **What makes a read due.** Reads aren't polled: a read is due only when
//! something says its table went stale.
//!
//! | What happened | What's due |
//! | --- | --- |
//! | The character entered the game (first `Room.Info`) | Every `[[command]]`, in declared order |
//! | A rule named `<reader>.stale` matched | That reader's command |
//! | Gear went on or came off (a Gear `wear` or `remove` rule) | Commands whose reader is the Gear point's |
//! | The player pressed Refresh | Commands whose reader is the Character point's |
//!
//! A table the player asked for counts as a read, so it's not asked for again.
//!
//! **When a due read goes out.** One at a time, each after the last was
//! answered, and only while all of these hold:
//!
//! - the character is in the game and has a position;
//! - the player has sent nothing for [`IDLE_AFTER`] and isn't typing;
//! - the character isn't fighting, asleep or incapacitated;
//! - the server isn't asking for a password;
//! - [`READ_SPACING`] has passed since the last read, and [`SAME_READ_SPACING`]
//!   since this table was last read.
//!
//! The prototype sent these in one burst whenever they fell due, with no
//! spacing, so this is never a faster rate than its. It differs in what's due
//! in three ways: a plugin can't say a table only changes with level, so each
//! connection reads them all once; a level-up reads one table more, and
//! Refresh asks for the Character point's whole set; and an improved skill is
//! read when idle, not when a panel opens.
//!
//! **What stays quiet.** The game answers commands in order, so with a read
//! out the next table from its reader is its answer. That table's lines, the
//! prompt that ends it and blank lines before it stay out of the chat, and
//! fire no trigger; a rule named `<reader>.none` marks a one-line answer that
//! isn't a table. Anything else that arrives shows as usual, and a table the
//! player asks for after the answer shows too. An answer that hasn't come
//! after [`ANSWER_WAIT`] is given up on.

use crate::persist::Timestamp;
use crate::plugin::{PluginEvent, PluginHost, Point};

/// How long the player must have sent nothing. Long enough that walking and
/// fighting by hand aren't interrupted, and that a change of gear has settled.
pub(super) const IDLE_AFTER: i64 = 5_000;
/// Between any two reads.
pub(super) const READ_SPACING: i64 = 2_000;
/// Between two reads of one table: however often it goes stale.
pub(super) const SAME_READ_SPACING: i64 = 30_000;
/// The prototype gave up on an answer after this long.
pub(super) const ANSWER_WAIT: i64 = 6_000;
/// Positions a game's GMCP gives when a command is unwelcome or can't run, by
/// how the word starts: `Fight`, `Sleep`, `STUN!`, `DYING`.
const BUSY_POSITIONS: [&str; 6] = ["fight", "sleep", "stun", "dying", "dead", "incap"];

/// The last part of a table's event names when its table ended, and of the
/// rules that say something about a read.
const ENDED: &str = "end";
const STARTED: &str = "start";
const CUT: &str = "cut";
const STALE: &str = "stale";
const NO_TABLE: &str = "none";

#[derive(Clone, Debug, PartialEq, Eq)]
struct Read {
    command: String,
    reader: String,
    /// Which point's table answers it.
    point: Point,
    due: bool,
    last_read: Option<Timestamp>,
}

/// How far an answer has come.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Answer {
    /// Sent, and nothing of it seen.
    Awaited { read: usize },
    /// Its table is arriving.
    Arriving { read: usize },
    /// Its one line came, and its prompt should follow.
    Closing,
}

/// What stands in the way of a read, read fresh for each attempt.
#[derive(Clone, Copy, Debug)]
pub(super) struct Moment<'a> {
    pub(super) typing: bool,
    pub(super) echo_off: bool,
    pub(super) in_fight: bool,
    pub(super) position: Option<&'a str>,
}

/// A line, as far as quiet reads care.
#[derive(Clone, Copy, Debug)]
pub(super) struct Seen<'a> {
    pub(super) events: &'a [PluginEvent],
    /// The line holds a prompt.
    pub(super) prompt: bool,
    /// Text follows the prompt, or there's no prompt and the line is blank.
    pub(super) glued: bool,
    pub(super) blank: bool,
}

/// One connection's quiet reads.
#[derive(Debug, Default)]
pub(super) struct QuietReads {
    reads: Vec<Read>,
    /// The names of the Gear point's `wear` and `remove` rules.
    gear_changes: Vec<String>,
    in_game: bool,
    last_player_command: Option<Timestamp>,
    last_sent: Option<Timestamp>,
    answer: Option<(Answer, Timestamp)>,
}

impl QuietReads {
    /// Takes up what the enabled plugins declare. A read already known keeps
    /// its state; one new to this connection is due if the character is in
    /// the game.
    pub(super) fn declare(&mut self, plugins: &PluginHost) {
        let mut reads = Vec::new();
        let mut gear_changes = Vec::new();
        for plugin in plugins.enabled() {
            let readers = plugin.points.readers();
            let commands = plugin.points.character.iter().flat_map(|c| &c.commands);
            for command in commands {
                let point = (readers.iter())
                    .find(|(_, reader)| reader.name == command.reader)
                    .map_or(Point::Character, |(point, _)| *point);
                let known = self
                    .reads
                    .iter()
                    .find(|read| read.command == command.command && read.reader == command.reader);
                reads.push(known.cloned().unwrap_or_else(|| Read {
                    command: command.command.clone(),
                    reader: command.reader.clone(),
                    point,
                    due: self.in_game,
                    last_read: None,
                }));
            }
            let gear = plugin.points.gear.iter();
            let rules = gear.flat_map(|gear| gear.wear.iter().chain(&gear.remove));
            gear_changes.extend(rules.map(|rule| rule.name.as_str().to_owned()));
        }
        self.reads = reads;
        self.gear_changes = gear_changes;
        self.answer = None;
    }

    /// The command that reads `reader`'s table, for saying where data came
    /// from.
    pub(super) fn command_of(&self, reader: &str) -> Option<&str> {
        let read = self.reads.iter().find(|read| read.reader == reader)?;
        Some(&read.command)
    }

    /// The character is in the game: everything's stale. Once a connection.
    pub(super) fn entered_game(&mut self) {
        if !std::mem::replace(&mut self.in_game, true) {
            self.reads.iter_mut().for_each(|read| read.due = true);
        }
    }

    /// The player sent something.
    pub(super) fn player_acted(&mut self, now: Timestamp) {
        self.last_player_command = Some(now);
    }

    /// The player asked for the character's tables again.
    pub(super) fn refresh(&mut self) {
        for read in &mut self.reads {
            read.due |= read.point == Point::Character;
        }
    }

    /// Something changed what the Gear point's tables say.
    pub(super) fn gear_changed(&mut self) {
        for read in &mut self.reads {
            read.due |= read.point == Point::Gear;
        }
    }

    /// Whether a read is on its way or waiting for its moment.
    pub(super) fn is_busy(&self) -> bool {
        self.answer.is_some() || self.reads.iter().any(|read| read.due)
    }

    /// Whether the text arriving now is an answer's.
    pub(super) fn is_hiding(&self) -> bool {
        matches!(
            self.answer,
            Some((Answer::Arriving { .. } | Answer::Closing, _))
        )
    }

    /// The command to send now, if any. It counts as sent.
    pub(super) fn next(&mut self, moment: Moment<'_>, now: Timestamp) -> Option<String> {
        self.give_up_on_answer(now);
        if self.answer.is_some() || !self.game_can_spare_one(moment, now) {
            return None;
        }
        let spaced = |last: Option<Timestamp>, by: i64| last.is_none_or(|at| now.0 - at.0 >= by);
        if !spaced(self.last_sent, READ_SPACING) {
            return None;
        }
        let at = (self.reads.iter())
            .position(|read| read.due && spaced(read.last_read, SAME_READ_SPACING))?;
        let read = &mut self.reads[at];
        read.due = false;
        read.last_read = Some(now);
        self.last_sent = Some(now);
        self.answer = Some((Answer::Awaited { read: at }, now));
        Some(read.command.clone())
    }

    /// Reads a line's events, and says whether it stays out of the chat.
    pub(super) fn read_line(&mut self, seen: Seen<'_>, now: Timestamp) -> bool {
        self.give_up_on_answer(now);
        let hidden = self.hides(seen, now);
        for event in seen.events {
            if event.point == Point::Gear && self.gear_changes.iter().any(|n| *n == *event.name) {
                self.gear_changed();
            }
            let Some((reader, part)) = event.name.rsplit_once('.') else {
                continue;
            };
            let read = self.reads.iter_mut().find(|read| read.reader == reader);
            let Some(read) = read else { continue };
            match part {
                ENDED => {
                    read.due = false;
                    read.last_read = Some(now);
                }
                // A read's own text can't make it stale, or it would go on
                // asking for itself.
                STALE if !hidden => read.due = true,
                _ => {}
            }
        }
        hidden
    }

    fn game_can_spare_one(&self, moment: Moment<'_>, now: Timestamp) -> bool {
        let idle = (self.last_player_command).is_none_or(|at| now.0 - at.0 >= IDLE_AFTER);
        let at_ease = moment.position.is_some_and(|position| {
            let position = position.trim().to_lowercase();
            !position.is_empty() && !BUSY_POSITIONS.iter().any(|busy| position.starts_with(busy))
        });
        self.in_game && idle && at_ease && !moment.typing && !moment.echo_off && !moment.in_fight
    }

    fn give_up_on_answer(&mut self, now: Timestamp) {
        if self
            .answer
            .is_some_and(|(_, since)| now.0 - since.0 >= ANSWER_WAIT)
        {
            self.answer = None;
        }
    }

    fn hides(&mut self, seen: Seen<'_>, now: Timestamp) -> bool {
        let named = |read: usize, part: &str| {
            let reader = &self.reads[read].reader;
            seen.events.iter().any(|event| {
                event
                    .name
                    .rsplit_once('.')
                    .is_some_and(|(name, ending)| name == reader && ending == part)
            })
        };
        let Some((answer, _)) = self.answer else {
            return false;
        };
        match answer {
            Answer::Awaited { read } if named(read, STARTED) => {
                let done = named(read, ENDED) || named(read, CUT);
                self.answer = (!done).then_some((Answer::Arriving { read }, now));
                true
            }
            Answer::Awaited { read } if named(read, NO_TABLE) => {
                self.answer = Some((Answer::Closing, now));
                true
            }
            Answer::Awaited { .. } => seen.blank,
            Answer::Arriving { read } if named(read, ENDED) || named(read, CUT) => {
                self.answer = None;
                !(seen.prompt && seen.glued)
            }
            Answer::Arriving { .. } => {
                self.answer = self.answer.map(|(answer, _)| (answer, now));
                true
            }
            Answer::Closing => {
                self.answer = None;
                seen.blank || (seen.prompt && !seen.glued)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::line::Line;
    use crate::plugin::Plugin;
    use std::sync::Arc;

    const AT_EASE: Moment<'static> = Moment {
        typing: false,
        echo_off: false,
        in_fight: false,
        position: Some("Stand"),
    };

    fn plugin() -> Plugin {
        Plugin::from_files(|file| {
            Ok(match file {
                "plugin.toml" => "id = \"game\"\nname = \"Game\"\nversion = \"0.1.0\"\napi = 1\n\
                    license = \"MIT\"\nlayer = \"game\"\n[servers]\nhosts = [\"game.example\"]\n\
                    [points]\ncharacter = \"character.toml\"\ngear = \"gear.toml\"\n"
                    .to_owned(),
                "gear.toml" => "[equipment]\nname = \"worn\"\nstart = '''^Worn:'''\n\
                    row = '''^<'''\n\
                    [[wear]]\nname = \"gear.wore\"\npattern = '''^You wear'''\n"
                    .to_owned(),
                _ => "[[command]]\ncommand = \"stats\"\nreader = \"stats\"\n\
                    [[command]]\ncommand = \"spells\"\nreader = \"spells\"\n\
                    [[command]]\ncommand = \"gear\"\nreader = \"worn\"\n\
                    [[reader]]\nname = \"stats\"\nstart = '''^Stats'''\nrow = '''^:'''\n\
                    [[reader]]\nname = \"spells\"\nstart = '''^Spells'''\nrow = '''^:'''\n"
                    .to_owned(),
            })
        })
        .expect("the plugin loads")
    }

    fn reads() -> QuietReads {
        let mut reads = QuietReads::default();
        reads.declare(&PluginHost::new([plugin()]).expect("it compiles"));
        reads.entered_game();
        reads
    }

    fn event(name: &str) -> PluginEvent {
        PluginEvent {
            plugin: plugin().id().clone(),
            point: Point::Character,
            name: Arc::from(name),
            captures: Box::default(),
            line: Line::command(""),
            hook: None,
            map_signal: None,
        }
    }

    fn line(events: &[PluginEvent]) -> Seen<'_> {
        Seen {
            events,
            prompt: false,
            glued: false,
            blank: false,
        }
    }

    fn prompt(events: &[PluginEvent]) -> Seen<'_> {
        Seen {
            prompt: true,
            ..line(events)
        }
    }

    /// Answers the read out, with a table and its prompt.
    fn answer(reads: &mut QuietReads, reader: &str, now: Timestamp) {
        assert!(reads.read_line(line(&[event(&format!("{reader}.start"))]), now));
        assert!(reads.read_line(prompt(&[event(&format!("{reader}.end"))]), now));
    }

    #[test]
    fn entering_the_game_reads_every_command_in_order_spaced_apart() {
        let mut reads = reads();
        let mut sent = Vec::new();
        for second in 0..60 {
            let now = Timestamp(second * 1_000);
            if let Some(command) = reads.next(AT_EASE, now) {
                let reader = if command == "gear" {
                    "worn"
                } else {
                    command.as_str()
                };
                answer(&mut reads, reader, now);
                sent.push((command, now.0));
            }
        }
        let expected = [("stats", 0), ("spells", 2_000), ("gear", 4_000)]
            .map(|(command, at)| (command.to_owned(), at));
        assert_eq!(sent, expected);
    }

    #[test]
    fn a_read_waits_for_the_last_ones_answer() {
        let mut reads = reads();
        assert_eq!(reads.next(AT_EASE, Timestamp(0)).unwrap(), "stats");
        assert_eq!(reads.next(AT_EASE, Timestamp(5_000)), None, "still out");
        assert_eq!(
            reads.next(AT_EASE, Timestamp(ANSWER_WAIT)).unwrap(),
            "spells",
            "given up on"
        );
    }

    #[test]
    fn nothing_goes_out_before_the_game_or_without_a_position() {
        let mut reads = reads();
        reads.in_game = false;
        assert_eq!(reads.next(AT_EASE, Timestamp(0)), None);
        reads.in_game = true;
        for position in [None, Some(""), Some("  ")] {
            let moment = Moment {
                position,
                ..AT_EASE
            };
            assert_eq!(reads.next(moment, Timestamp(0)), None, "{position:?}");
        }
    }

    #[test]
    fn typing_a_fight_a_password_or_sleep_stops_the_reads() {
        let mut reads = reads();
        for (moment, why) in [
            (
                Moment {
                    typing: true,
                    ..AT_EASE
                },
                "typing",
            ),
            (
                Moment {
                    echo_off: true,
                    ..AT_EASE
                },
                "a password",
            ),
            (
                Moment {
                    in_fight: true,
                    ..AT_EASE
                },
                "a target",
            ),
            (
                Moment {
                    position: Some("Fight"),
                    ..AT_EASE
                },
                "fighting",
            ),
            (
                Moment {
                    position: Some("Sleep"),
                    ..AT_EASE
                },
                "asleep",
            ),
            (
                Moment {
                    position: Some("STUN!"),
                    ..AT_EASE
                },
                "stunned",
            ),
            (
                Moment {
                    position: Some("DYING"),
                    ..AT_EASE
                },
                "dying",
            ),
        ] {
            assert_eq!(reads.next(moment, Timestamp(0)), None, "{why}");
        }
        assert!(reads.next(AT_EASE, Timestamp(0)).is_some(), "they waited");
    }

    #[test]
    fn a_read_waits_until_the_player_has_been_idle() {
        let mut reads = reads();
        reads.player_acted(Timestamp(10_000));
        assert_eq!(reads.next(AT_EASE, Timestamp(14_999)), None);
        assert!(reads.next(AT_EASE, Timestamp(15_000)).is_some());
    }

    #[test]
    fn however_often_a_table_goes_stale_it_is_read_at_a_spaced_rate() {
        let mut reads = reads();
        for read in &mut reads.reads {
            read.due = read.reader == "stats";
        }
        let mut sent = Vec::new();
        let stale = [event("stats.stale")];
        for second in 0..300 {
            let now = Timestamp(second * 1_000);
            reads.read_line(line(&stale), now);
            if reads.next(AT_EASE, now).is_some() {
                answer(&mut reads, "stats", now);
                sent.push(now.0);
            }
        }
        assert_eq!(sent.len(), 10);
        assert!(
            sent.windows(2)
                .all(|two| two[1] - two[0] >= SAME_READ_SPACING)
        );
    }

    #[test]
    fn a_table_the_player_asked_for_counts_as_read() {
        let mut reads = reads();
        for read in &mut reads.reads {
            read.due = read.reader == "stats";
        }
        assert!(!reads.read_line(line(&[event("stats.start")]), Timestamp(0)));
        assert!(!reads.read_line(prompt(&[event("stats.end")]), Timestamp(0)));
        assert!(!reads.is_busy());
    }

    #[test]
    fn refresh_asks_again_for_the_character_and_a_gear_change_for_the_gear() {
        let mut reads = reads();
        reads.reads.iter_mut().for_each(|read| read.due = false);
        reads.refresh();
        let due = |reads: &QuietReads| -> Vec<String> {
            let due = reads.reads.iter().filter(|read| read.due);
            due.map(|read| read.command.clone()).collect()
        };
        assert_eq!(due(&reads), ["stats", "spells"]);
        reads.reads.iter_mut().for_each(|read| read.due = false);
        reads.gear_changed();
        assert_eq!(due(&reads), ["gear"]);
    }

    #[test]
    fn an_answers_table_prompt_and_leading_blanks_are_hidden_and_nothing_else() {
        let mut reads = reads();
        let at = Timestamp(0);
        reads.next(AT_EASE, at);
        let blank = Seen {
            blank: true,
            ..line(&[])
        };
        assert!(reads.read_line(blank, at), "before the table");
        assert!(!reads.read_line(line(&[]), at), "someone speaks");
        assert!(!reads.is_hiding());
        assert!(reads.read_line(line(&[event("stats.start")]), at));
        assert!(reads.is_hiding());
        assert!(reads.read_line(line(&[event("stats.row")]), at));
        assert!(reads.read_line(line(&[]), at), "a table's ruled line");
        assert!(reads.read_line(prompt(&[event("stats.end")]), at));
        assert!(!reads.is_hiding());
        assert!(
            !reads.read_line(line(&[event("stats.start")]), at),
            "theirs"
        );
    }

    #[test]
    fn a_line_glued_after_the_ending_prompt_shows() {
        let mut reads = reads();
        reads.next(AT_EASE, Timestamp(0));
        reads.read_line(line(&[event("stats.start")]), Timestamp(0));
        let ended = [event("stats.end")];
        let glued = Seen {
            glued: true,
            ..prompt(&ended)
        };
        assert!(!reads.read_line(glued, Timestamp(0)));
    }

    #[test]
    fn a_one_line_answer_and_its_prompt_are_hidden() {
        let mut reads = reads();
        reads.next(AT_EASE, Timestamp(0));
        assert!(reads.read_line(line(&[event("stats.none")]), Timestamp(0)));
        assert!(reads.read_line(prompt(&[]), Timestamp(0)));
        assert!(!reads.read_line(prompt(&[]), Timestamp(0)), "the next");
    }

    #[test]
    fn a_table_that_stalls_stops_hiding_what_follows() {
        let mut reads = reads();
        reads.next(AT_EASE, Timestamp(0));
        reads.read_line(line(&[event("stats.start")]), Timestamp(1_000));
        assert!(reads.read_line(line(&[]), Timestamp(6_000)));
        assert!(!reads.read_line(line(&[]), Timestamp(12_000)));
    }

    #[test]
    fn a_reads_own_text_cant_make_it_stale() {
        let mut reads = reads();
        reads.reads.iter_mut().for_each(|read| read.due = false);
        reads.refresh();
        reads.reads[1].due = false;
        reads.next(AT_EASE, Timestamp(0));
        reads.read_line(line(&[event("stats.start")]), Timestamp(0));
        reads.read_line(line(&[event("stats.stale")]), Timestamp(0));
        reads.read_line(prompt(&[event("stats.end")]), Timestamp(0));
        assert!(!reads.reads[0].due);
    }

    #[test]
    fn wearing_something_makes_the_gear_table_stale() {
        let mut reads = reads();
        reads.reads.iter_mut().for_each(|read| read.due = false);
        let mut wore = event("gear.wore");
        wore.point = Point::Gear;
        reads.read_line(line(&[wore]), Timestamp(0));
        assert!(reads.reads[2].due);
        assert!(!reads.reads[0].due);
    }
}
