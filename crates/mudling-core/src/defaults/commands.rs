//! Command chaining and speedwalk, as the prototype's `CommandChain` and
//! `Speedwalk` did them, with the game's syntax from the Commands point.

use crate::plugin::PluginHost;
use crate::plugin::points::Speedwalk;

/// What the player types between chained commands: `n;n;open s`.
pub const CHAIN_SEPARATOR: char = ';';
/// Games cut input lines off (256 bytes is common); this leaves room.
pub const LONGEST_LINE: usize = 200;
/// What opens a door when no plugin names the game's command.
const OPEN_DOOR: &str = "open {dir}";

/// The commands in a typed chain, trimmed, empty ones dropped.
pub fn split_chain(typed: &str) -> Vec<&str> {
    typed.split(CHAIN_SEPARATOR).map(str::trim).filter(|command| !command.is_empty()).collect()
}

/// The lines that send `commands`. With no separator each command is its
/// own line, which is always safe. With one, they're joined by it, as many
/// to a line as fit in [`LONGEST_LINE`].
pub fn chain_lines(commands: &[impl AsRef<str>], separator: Option<&str>) -> Vec<String> {
    let commands = commands.iter().map(AsRef::as_ref);
    let Some(separator) = separator.filter(|separator| !separator.is_empty()) else {
        return commands.map(str::to_owned).collect();
    };
    let mut lines = Vec::new();
    let mut line = String::new();
    for command in commands {
        if !line.is_empty() && line.len() + separator.len() + command.len() > LONGEST_LINE {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push_str(separator);
        }
        line.push_str(command);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    North,
    East,
    South,
    West,
    Up,
    Down,
}

impl Direction {
    fn from_letter(letter: char) -> Option<Direction> {
        Some(match letter.to_ascii_lowercase() {
            'n' => Direction::North,
            'e' => Direction::East,
            's' => Direction::South,
            'w' => Direction::West,
            'u' => Direction::Up,
            'd' => Direction::Down,
            _ => return None,
        })
    }

    pub fn word(self) -> &'static str {
        match self {
            Direction::North => "north",
            Direction::East => "east",
            Direction::South => "south",
            Direction::West => "west",
            Direction::Up => "up",
            Direction::Down => "down",
        }
    }

    fn letter(self) -> &'static str {
        &self.word()[..1]
    }
}

/// One step of a speedwalk: `3n` is three steps north; `[s]` opens the door
/// south, then goes through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Step {
    pub count: u32,
    pub direction: Direction,
    pub door: bool,
}

/// How the game takes a walk: its speedwalk syntax and door command, the
/// highest layer's of each.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WalkSyntax {
    pub speedwalk: Option<Speedwalk>,
    pub open_door: Option<String>,
}

impl WalkSyntax {
    pub fn of(host: &PluginHost) -> WalkSyntax {
        let mut syntax = WalkSyntax::default();
        for (_, commands) in host.declared(|points| points.commands.as_ref()) {
            if let Some(speedwalk) = &commands.speedwalk {
                syntax.speedwalk = Some(speedwalk.clone());
            }
            if let Some(open) = commands.doors.get("open") {
                syntax.open_door = Some(open.clone());
            }
        }
        syntax
    }

    /// The commands `typed` stands for, or `None` when it isn't speedwalk
    /// (a lone `n` is just a move). Without the game's syntax each move is
    /// its own command; with it, runs between doors go as one command.
    pub fn expand(&self, typed: &str) -> Option<Vec<String>> {
        let steps = parse_speedwalk(typed)?;
        let open = |direction: Direction| self.open_door.as_deref().unwrap_or(OPEN_DOOR).replace("{dir}", direction.word());
        let mut commands = Vec::new();
        let mut runs: Vec<(u32, Direction)> = Vec::new();
        for step in steps {
            if step.door {
                self.flush(&mut runs, &mut commands);
                commands.push(open(step.direction));
            }
            match &self.speedwalk {
                None => commands.extend(std::iter::repeat_n(step.direction.word().to_owned(), step.count as usize)),
                Some(_) => match runs.last_mut() {
                    Some((count, direction)) if *direction == step.direction => *count += step.count,
                    _ => runs.push((step.count, step.direction)),
                },
            }
        }
        self.flush(&mut runs, &mut commands);
        Some(commands)
    }

    /// Writes `runs` as one command in the game's syntax.
    fn flush(&self, runs: &mut Vec<(u32, Direction)>, commands: &mut Vec<String>) {
        let Some(speedwalk) = &self.speedwalk else { return };
        if runs.is_empty() {
            return;
        }
        let steps: Vec<String> = runs
            .drain(..)
            .map(|(count, direction)| {
                speedwalk.step.replace("{count}", &count.to_string()).replace("{dir}", direction.letter())
            })
            .collect();
        commands.push(steps.join(&speedwalk.join));
    }
}

/// Reads speedwalk shorthand: `3n2e`, `8n, 7w, [s], d`. Steps are a count
/// of up to two digits, then a direction letter, bracketed for a door. Runs
/// may be written together only with counts or brackets, so a word such as
/// `news` or `sw` isn't taken for one.
pub fn parse_speedwalk(typed: &str) -> Option<Vec<Step>> {
    let tokens: Vec<&str> = typed.split(|c: char| c == ',' || c.is_whitespace()).filter(|t| !t.is_empty()).collect();
    let marked = |token: &str| token.starts_with(|c: char| c.is_ascii_digit() || c == '[');
    if !(tokens.len() > 1 || tokens.first().is_some_and(|token| marked(token))) {
        return None;
    }
    let mut steps = Vec::new();
    for token in tokens {
        let first = steps.len();
        let mut all_marked = true;
        let mut rest = token;
        while !rest.is_empty() {
            let (step, after) = parse_step(rest)?;
            all_marked &= marked(rest);
            steps.push(step);
            rest = after;
        }
        if steps.len() - first > 1 && !all_marked {
            return None;
        }
    }
    Some(steps)
}

/// One step at the start of `text`, and the text after it.
fn parse_step(text: &str) -> Option<(Step, &str)> {
    let digits = text.bytes().take_while(u8::is_ascii_digit).count();
    if digits > 2 {
        return None;
    }
    let count = text[..digits].parse::<u32>().map_or(1, |count| count.max(1));
    let rest = &text[digits..];
    let (door, rest) = match rest.strip_prefix('[') {
        Some(rest) => (true, rest),
        None => (false, rest),
    };
    let letter = rest.chars().next()?;
    let direction = Direction::from_letter(letter)?;
    let rest = &rest[1..];
    let rest = if door { rest.strip_prefix(']')? } else { rest };
    Some((Step { count, direction, door }, rest))
}
