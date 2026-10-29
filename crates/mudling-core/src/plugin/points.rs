//! The nine extension points of plugin API v1 (PRD §11), as typed
//! declarations read from a plugin's TOML files.
//!
//! The host runs [`Text`]. The other eight are parsed, validated and exposed
//! through [`crate::plugin::PluginHost::declared`] for the tickets that give
//! them behavior. Every pattern in every point compiles at load, so a broken
//! plugin is rejected before it runs.

use crate::ansi::{Color, Style};
use regex::Regex;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

/// Declares [`Point`], [`Point::ALL`] and [`Point::name`] from one list, so
/// a new point can't be left out of any of them.
macro_rules! points {
    ($($point:ident => $name:literal),+ $(,)?) => {
        /// An extension point, as named in `plugin.toml`'s `[points]` table.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum Point {
            $($point),+
        }

        impl Point {
            pub const ALL: [Point; [$($name),+].len()] = [$(Point::$point),+];

            pub fn name(self) -> &'static str {
                match self {
                    $(Point::$point => $name),+
                }
            }
        }
    };
}

points! {
    Connection => "connection",
    Commands => "commands",
    Text => "text",
    Hud => "hud",
    Map => "map",
    Character => "character",
    Gear => "gear",
    Mobs => "mobs",
    Ui => "ui",
}

impl fmt::Display for Point {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Everything one plugin declares, one entry per extension point it extends.
#[derive(Clone, Debug, Default)]
pub struct Points {
    pub connection: Option<Connection>,
    pub commands: Option<Commands>,
    pub text: Option<Text>,
    pub hud: Option<Hud>,
    pub map: Option<Map>,
    pub character: Option<Character>,
    pub gear: Option<Gear>,
    pub mobs: Option<Mobs>,
    pub ui: Option<Ui>,
}

/// A regex over a line's plain text, compiled once at load. Rust's `regex`
/// matches in linear time, so no pattern can hang the client.
#[derive(Clone, Debug, Deserialize)]
#[serde(try_from = "String")]
pub struct Pattern(Regex);

impl Pattern {
    pub fn new(source: &str) -> Result<Pattern, regex::Error> {
        Regex::new(source).map(Pattern)
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    pub fn regex(&self) -> &Regex {
        &self.0
    }

    fn has_group(&self, name: &str) -> bool {
        self.0.capture_names().flatten().any(|group| group == name)
    }
}

impl TryFrom<String> for Pattern {
    type Error = String;

    fn try_from(source: String) -> Result<Pattern, String> {
        Pattern::new(&source).map_err(|error| format!("the pattern doesn't compile: {error}"))
    }
}

impl PartialEq for Pattern {
    fn eq(&self, other: &Pattern) -> bool {
        self.as_str() == other.as_str()
    }
}

/// The name of an event a pattern emits, such as `tell` or `skill.improved`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Deserialize)]
#[serde(try_from = "String")]
pub struct EventName(Arc<str>);

impl EventName {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn shared(&self) -> Arc<str> {
        self.0.clone()
    }
}

impl TryFrom<String> for EventName {
    type Error = String;

    fn try_from(name: String) -> Result<EventName, String> {
        let valid = !name.is_empty()
            && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'));
        if valid {
            Ok(EventName(name.into()))
        } else {
            Err(format!("`{name}` isn't an event name: use letters, digits, `_`, `.` and `-`"))
        }
    }
}

/// The Luau function a pattern hands its match to. VYT-197 runs it; the host
/// only names it on the [`crate::plugin::PluginEvent`] the match becomes.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Deserialize)]
#[serde(try_from = "String")]
pub struct HookName(Arc<str>);

impl HookName {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for HookName {
    type Error = String;

    fn try_from(name: String) -> Result<HookName, String> {
        let mut chars = name.chars();
        let valid = chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
        if valid {
            Ok(HookName(name.into()))
        } else {
            Err(format!("`{name}` isn't a Luau function name"))
        }
    }
}

/// A pattern that emits a named event, its named captures as the data.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventRule {
    pub name: EventName,
    pub pattern: Pattern,
    pub hook: Option<HookName>,
}

/// Reads a table the game prints: the line that starts it, each row, and the
/// line that ends it (or the next prompt).
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TableReader {
    pub name: String,
    pub start: Pattern,
    pub row: Pattern,
    pub end: Option<Pattern>,
    pub hook: Option<HookName>,
}

// ---- Connection ----

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Connection {
    /// GMCP modules to subscribe to, such as `"Char 1"`.
    #[serde(default)]
    pub gmcp: Vec<String>,
    #[serde(default, rename = "login")]
    pub logins: Vec<LoginPrompt>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoginPrompt {
    pub pattern: Pattern,
    pub send: LoginReply,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LoginReply {
    Name,
    Password,
    /// An empty line, to get past a banner.
    Blank,
}

// ---- Commands ----

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Commands {
    /// What joins chained commands on one line. Only one applies: the
    /// highest layer's.
    pub separator: Option<String>,
    /// The game's word for each verb, such as `remove = "remove"`.
    #[serde(default)]
    pub verbs: BTreeMap<String, String>,
    /// Door commands, `{dir}` standing for the direction: `open = "open {dir}"`.
    #[serde(default)]
    pub doors: BTreeMap<String, String>,
    pub speedwalk: Option<Speedwalk>,
    #[serde(default, rename = "alias")]
    pub aliases: Vec<Alias>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Speedwalk {
    /// One run of steps, with `{count}` and `{dir}`: `"{count}{dir}"`.
    pub step: String,
    /// What goes between runs.
    #[serde(default)]
    pub join: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Alias {
    pub name: String,
    pub expansion: String,
}

// ---- Text ----

/// Text: the one point the host runs itself.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Text {
    /// Only one applies: the highest layer's.
    pub pager: Option<Pager>,
    /// Patterns that recognize a prompt in text the server hasn't ended,
    /// for servers that send no GA.
    #[serde(default, rename = "prompt")]
    pub prompts: Vec<PromptHint>,
    /// Patterns over each finished line, trigger packs included.
    #[serde(default, rename = "pattern")]
    pub patterns: Vec<EventRule>,
    /// Coloring for lines the server sends without any.
    #[serde(default, rename = "color")]
    pub colors: Vec<ColorRule>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pager {
    /// Matches the pager prompt, which arrives without a line end.
    pub prompt: Pattern,
    /// What to send to continue; empty sends a blank line.
    #[serde(default)]
    pub key: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromptHint {
    pub pattern: Pattern,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ColorRule {
    pub pattern: Pattern,
    /// Palette indexes, as in [`Color`].
    pub fg: Option<u8>,
    pub bg: Option<u8>,
    #[serde(default)]
    pub bold: bool,
    /// Colors only this named capture, not the whole match.
    pub group: Option<String>,
}

impl ColorRule {
    pub fn style(&self) -> Style {
        Style { fg: self.fg.map(Color), bg: self.bg.map(Color), bold: self.bold }
    }
}

impl Text {
    pub(crate) fn validate(&self) -> Result<(), String> {
        for color in &self.colors {
            if color.style().is_plain() {
                return Err(format!("the color rule `{}` sets no fg, bg or bold", color.pattern.as_str()));
            }
            if let Some(group) = &color.group
                && !color.pattern.has_group(group)
            {
                return Err(format!(
                    "the color rule `{}` names the group `{group}`, which the pattern doesn't capture",
                    color.pattern.as_str()
                ));
            }
        }
        Ok(())
    }
}

// ---- HUD ----

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hud {
    /// Each vital and the GMCP field that holds it: `hp = "Char.Vitals.hp"`.
    #[serde(default)]
    pub vitals: BTreeMap<String, String>,
    /// The opponent bar's fields, the same way.
    #[serde(default)]
    pub opponent: BTreeMap<String, String>,
    /// The GMCP message that lists affects.
    pub affects: Option<String>,
}

// ---- Map ----

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Map {
    /// Only one applies: the highest layer's.
    pub room_identity: Option<RoomIdentity>,
    /// Area names by the game's zone id.
    #[serde(default)]
    pub areas: BTreeMap<String, String>,
    #[serde(default)]
    pub exits: Vec<EventRule>,
    #[serde(default)]
    pub failed_move: Vec<EventRule>,
    #[serde(default)]
    pub darkness: Vec<EventRule>,
    #[serde(default)]
    pub flee: Vec<EventRule>,
    /// Areas whose exits don't hold still.
    #[serde(default)]
    pub mazes: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RoomIdentity {
    /// The room number GMCP sends.
    Gmcp,
    /// The room's name, exits and description.
    Description,
}

// ---- Character ----

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Character {
    #[serde(default, rename = "command")]
    pub commands: Vec<BackgroundCommand>,
    #[serde(default, rename = "reader")]
    pub readers: Vec<TableReader>,
    #[serde(default)]
    pub skill_improved: Vec<EventRule>,
}

/// A command the client may send quietly, and the reader for its reply.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackgroundCommand {
    pub command: String,
    pub reader: String,
}

impl Character {
    pub(crate) fn validate(&self) -> Result<(), String> {
        for command in &self.commands {
            if !self.readers.iter().any(|reader| reader.name == command.reader) {
                return Err(format!(
                    "the command `{}` names the reader `{}`, which isn't declared",
                    command.command, command.reader
                ));
            }
        }
        Ok(())
    }
}

// ---- Gear ----

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Gear {
    pub identify: Option<TableReader>,
    pub examine: Option<TableReader>,
    /// The worn-equipment table.
    pub equipment: Option<TableReader>,
    /// Wear slots, in the order the game lists them.
    #[serde(default)]
    pub slots: Vec<String>,
    #[serde(default)]
    pub wear: Vec<EventRule>,
    #[serde(default)]
    pub remove: Vec<EventRule>,
    #[serde(default)]
    pub put: Vec<EventRule>,
    /// Item keywords that are containers.
    #[serde(default)]
    pub containers: Vec<String>,
}

// ---- Mobs ----

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mobs {
    /// Messages from a mob that attacks on sight.
    #[serde(default)]
    pub aggro: Vec<EventRule>,
    /// Turns a room title into a mob name, captured as `name`.
    #[serde(default)]
    pub room_title: Vec<EventRule>,
}

// ---- UI ----

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ui {
    #[serde(default)]
    pub panels: Vec<String>,
    #[serde(default)]
    pub character_fields: Vec<String>,
}
