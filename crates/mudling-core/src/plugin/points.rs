//! The nine extension points of plugin API v1 (PRD §11), as typed
//! declarations read from a plugin's TOML files.
//!
//! The host runs the rules [`Points::event_rules`] lists and every
//! [`TableReader`] over each line. The rest, [`Mobs::room_title`] among it,
//! is exposed through [`crate::plugin::PluginHost::declared`] for what reads
//! it. Every pattern in every point compiles at load, so a broken plugin is
//! rejected before it runs.

use crate::ansi::{Color, Style};
use crate::line::Link;
use regex_automata::PatternID;
use regex_automata::meta;
use serde::Deserialize;
use std::cell::RefCell;
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

impl Points {
    /// Every rule the host matches over lines, with its point and what it
    /// tells the automap, Text's first. [`Mobs::room_title`] isn't one: it
    /// reads a room title, not a line.
    pub(super) fn event_rules<'a>(&'a self) -> Vec<(Point, &'a EventRule, Option<MapSignal>)> {
        let Points {
            connection: _,
            commands: _,
            text,
            hud: _,
            map,
            character,
            gear,
            mobs,
            ui: _,
        } = self;
        let mut rules = Vec::new();
        let mut add = |point, list: &'a [EventRule], signal| {
            rules.extend(list.iter().map(|rule| (point, rule, signal)));
        };
        if let Some(text) = text {
            add(Point::Text, &text.patterns, None);
        }
        if let Some(map) = map {
            add(Point::Map, &map.exits, Some(MapSignal::Exits));
            add(Point::Map, &map.failed_move, Some(MapSignal::FailedMove));
            add(Point::Map, &map.darkness, Some(MapSignal::Darkness));
            add(Point::Map, &map.flee, Some(MapSignal::Flee));
        }
        if let Some(character) = character {
            add(Point::Character, &character.skill_improved, None);
        }
        if let Some(gear) = gear {
            add(Point::Gear, &gear.wear, None);
            add(Point::Gear, &gear.remove, None);
            add(Point::Gear, &gear.put, None);
        }
        if let Some(mobs) = mobs {
            add(Point::Mobs, &mobs.aggro, Some(MapSignal::Aggressor));
        }
        rules
    }

    pub(crate) fn readers(&self) -> Vec<(Point, &TableReader)> {
        let character = self.character.iter().flat_map(|point| &point.readers);
        let gear = self
            .gear
            .iter()
            .flat_map(|gear| [&gear.identify, &gear.examine, &gear.equipment])
            .flatten();
        character
            .map(|reader| (Point::Character, reader))
            .chain(gear.map(|reader| (Point::Gear, reader)))
            .collect()
    }

    /// Checks what spans points: a background command's reader may be
    /// Character's or Gear's.
    pub(super) fn validate(&self) -> Result<(), String> {
        let readers = self.readers();
        let commands = self.character.iter().flat_map(|point| &point.commands);
        for command in commands {
            if !readers
                .iter()
                .any(|(_, reader)| reader.name == command.reader)
            {
                return Err(format!(
                    "the command `{}` names the reader `{}`, which isn't declared",
                    command.command, command.reader
                ));
            }
        }
        Ok(())
    }
}

/// What a rule from a list with a fixed meaning tells the automap, whatever
/// the plugin named its event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapSignal {
    /// The line lists the room's exits; its description follows.
    Exits,
    FailedMove,
    Darkness,
    /// The way is captured as `dir`.
    Flee,
    /// The mob is captured as `name`.
    Aggressor,
}

thread_local! {
    /// The `[fragments]` of the plugin being loaded. A [`Pattern`] compiles
    /// as it's parsed, out of reach of anything but the text it was given,
    /// so this is how the fragments get there.
    static FRAGMENTS: RefCell<BTreeMap<String, String>> = RefCell::default();
}

/// Runs `parse` with `fragments` expanded in every pattern it reads.
pub(super) fn with_fragments<T>(
    fragments: &BTreeMap<String, String>,
    parse: impl FnOnce() -> T,
) -> T {
    FRAGMENTS.set(fragments.clone());
    let parsed = parse();
    FRAGMENTS.take();
    parsed
}

/// Puts each declared fragment in place of its `{name}`, as a group so that
/// a `?` after it, or a `|` inside it, means the whole fragment. An escaped
/// `\{` is a literal brace, and a `{` that names no fragment is left for the
/// regex to read: `{2,}` is a repeat, and an unknown `{name}` doesn't compile.
fn expand(source: &str, fragments: &BTreeMap<String, String>) -> String {
    let mut expanded = String::with_capacity(source.len());
    let mut rest = source;
    while let Some(at) = rest.find(['\\', '{']) {
        let (before, from) = rest.split_at(at);
        expanded.push_str(before);
        if from.starts_with('\\') {
            let escape = from
                .char_indices()
                .nth(2)
                .map_or(from.len(), |(end, _)| end);
            expanded.push_str(&from[..escape]);
            rest = &from[escape..];
            continue;
        }
        let fragment = from[1..]
            .split_once('}')
            .and_then(|(name, after)| Some((fragments.get(name)?, after)));
        match fragment {
            Some((fragment, after)) => {
                expanded.push_str("(?:");
                expanded.push_str(fragment);
                expanded.push(')');
                rest = after;
            }
            None => {
                expanded.push('{');
                rest = &from[1..];
            }
        }
    }
    expanded.push_str(rest);
    expanded
}

/// A regex over a line's plain text, in the syntax of Rust's `regex` crate,
/// compiled once at load. It matches in linear time, so no pattern can hang
/// the client.
///
/// The engine is private, so it can change without changing the API. It is
/// built without the one-pass DFA: for patterns with Unicode classes (`\w`,
/// `\d`) that engine alone can cost hundreds of KB per pattern, and the
/// engines left extract captures just as well on lines this short.
#[derive(Clone, Debug, Deserialize)]
#[serde(try_from = "String")]
pub struct Pattern {
    source: Box<str>,
    regex: meta::Regex,
    /// Each named group and its index.
    names: Box<[(Arc<str>, usize)]>,
}

/// A named capture, as a byte range of the text matched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capture {
    pub name: Arc<str>,
    pub start: u32,
    pub end: u32,
}

impl Capture {
    /// What it captured of `text`, the text matched.
    pub fn text<'a>(&self, text: &'a str) -> &'a str {
        &text[self.start as usize..self.end as usize]
    }
}

/// Why a pattern doesn't compile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PatternError(String);

impl fmt::Display for PatternError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PatternError {}

impl Pattern {
    pub fn new(source: &str) -> Result<Pattern, PatternError> {
        let regex = meta::Regex::builder()
            .configure(meta::Config::new().onepass(false))
            .build(source)
            .map_err(|error| {
                PatternError(
                    error
                        .syntax_error()
                        .map_or_else(|| error.to_string(), ToString::to_string),
                )
            })?;
        let names = regex
            .group_info()
            .pattern_names(PatternID::ZERO)
            .enumerate()
            .filter_map(|(group, name)| Some((Arc::from(name?), group)))
            .collect();
        Ok(Pattern {
            source: source.into(),
            regex,
            names,
        })
    }

    pub fn as_str(&self) -> &str {
        &self.source
    }

    pub fn is_match(&self, text: &str) -> bool {
        self.regex.is_match(text)
    }

    /// The named captures of the first match in `text`, those that took part
    /// in it; `None` if the pattern doesn't match.
    pub fn captures(&self, text: &str) -> Option<Box<[Capture]>> {
        let mut found = self.regex.create_captures();
        self.regex.captures(text, &mut found);
        found.is_match().then(|| {
            self.names
                .iter()
                .filter_map(|(name, group)| {
                    let span = found.get_group(*group)?;
                    Some(Capture {
                        name: name.clone(),
                        start: span.start as u32,
                        end: span.end as u32,
                    })
                })
                .collect()
        })
    }

    /// The index of the named group.
    pub(crate) fn group(&self, name: &str) -> Option<usize> {
        self.names
            .iter()
            .find(|(group, _)| &**group == name)
            .map(|(_, index)| *index)
    }

    /// The byte range of `group` (0 is the whole match) in every match in
    /// `text`, skipping matches it took no part in.
    pub(crate) fn spans<'a>(
        &'a self,
        text: &'a str,
        group: usize,
    ) -> impl Iterator<Item = (usize, usize)> + 'a {
        self.regex
            .captures_iter(text)
            .filter_map(move |found| found.get_group(group).map(|span| (span.start, span.end)))
    }
}

impl TryFrom<String> for Pattern {
    type Error = String;

    fn try_from(source: String) -> Result<Pattern, String> {
        let source = FRAGMENTS.with_borrow(|fragments| expand(&source, fragments));
        Pattern::new(&source).map_err(|error| format!("the pattern doesn't compile: {error}"))
    }
}

/// One pattern or several, written `row = '''…'''` or `row = ['''…''', '''…''']`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(try_from = "OneOrMany")]
pub struct Patterns(Vec<Pattern>);

#[derive(Deserialize)]
#[serde(untagged)]
enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

impl TryFrom<OneOrMany> for Patterns {
    type Error = String;

    fn try_from(sources: OneOrMany) -> Result<Patterns, String> {
        let sources = match sources {
            OneOrMany::One(source) => vec![source],
            OneOrMany::Many(sources) => sources,
        };
        let patterns: Result<Vec<Pattern>, String> =
            sources.into_iter().map(Pattern::try_from).collect();
        patterns.map(Patterns)
    }
}

impl std::ops::Deref for Patterns {
    type Target = [Pattern];

    fn deref(&self) -> &[Pattern] {
        &self.0
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
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'));
        if valid {
            Ok(EventName(name.into()))
        } else {
            Err(format!(
                "`{name}` isn't an event name: use letters, digits, `_`, `.` and `-`"
            ))
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
        let valid = chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
        if valid {
            Ok(HookName(name.into()))
        } else {
            Err(format!("`{name}` isn't a Luau function name"))
        }
    }
}

/// A field of a GMCP message: the package, then the key after the last `.`,
/// as in `"Char.Status.room_exits"`. The package matches in any case.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub struct GmcpField {
    pub package: String,
    pub key: String,
}

impl GmcpField {
    /// Whether the field is in a message of `package`.
    pub fn is_in(&self, package: &str) -> bool {
        self.package.eq_ignore_ascii_case(package)
    }
}

impl TryFrom<String> for GmcpField {
    type Error = String;

    fn try_from(path: String) -> Result<GmcpField, String> {
        match path.rsplit_once('.') {
            Some((package, key)) if !package.is_empty() && !key.is_empty() => Ok(GmcpField {
                package: package.to_owned(),
                key: key.to_owned(),
            }),
            _ => Err(format!(
                "`{path}` isn't a GMCP field: write the package, then the key, as in \"Char.Status.str\""
            )),
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

/// Reads a table the game prints, as events to its hook:
///
/// - `<name>.start`: a line that opens it. One more start before any row is
///   the same table, such as a header under its title.
/// - `<name>.row`: a row. Row patterns run only while the table is open.
/// - `<name>.gap`: the first line after a row, or after the start, that's
///   no row.
/// - `<name>.end`: its `end` pattern, any prompt, or the plugin's next table.
/// - `<name>.cut`: it ended at a pager prompt no one answered, or with the
///   connection, so what was read of it isn't all of it.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TableReader {
    pub name: String,
    pub start: Patterns,
    pub row: Patterns,
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
    /// GMCP messages to send, which need `send_gmcp`.
    #[serde(default, rename = "request")]
    pub requests: Vec<GmcpRequest>,
}

/// A GMCP message the plugin sends, each time a field's value changes:
/// `send = "request area"` when `room.info.zone` does.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GmcpRequest {
    pub send: GmcpMessage,
    pub when: GmcpField,
}

/// A GMCP message from the client: a package name, then perhaps a space and
/// its data. It goes out only inside a GMCP subnegotiation, which a server
/// reads as GMCP, never on the line its command interpreter reads, so it
/// can't be a game command. It holds no line ends or other control
/// characters, and no `Core` message, which runs the connection itself.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub struct GmcpMessage {
    pub package: String,
    pub data: String,
}

impl fmt::Display for GmcpMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.data.as_str() {
            "" => f.write_str(&self.package),
            data => write!(f, "{} {data}", self.package),
        }
    }
}

impl TryFrom<String> for GmcpMessage {
    type Error = String;

    fn try_from(message: String) -> Result<GmcpMessage, String> {
        let (package, data) = message.split_once(' ').unwrap_or((&message, ""));
        let named = |part: &str| {
            part.starts_with(|c: char| c.is_ascii_alphabetic())
                && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        };
        if !package.split('.').all(named) {
            return Err(format!(
                "`{message}` isn't a GMCP message: start with a package name, such as \"request area\" or \"Char.Skills.Get\""
            ));
        }
        if message.chars().any(char::is_control) {
            return Err(format!(
                "the GMCP message `{message}` holds a control character"
            ));
        }
        let core = package.split('.').next().unwrap_or_default();
        if core.eq_ignore_ascii_case("core") {
            return Err(format!(
                "`{message}` is a Core message, which MUDling sends itself"
            ));
        }
        Ok(GmcpMessage {
            package: package.to_owned(),
            data: data.to_owned(),
        })
    }
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
    /// For this plugin's own hooks, through `mud.keyword`.
    pub keywords: Option<Keywords>,
}

/// Which word of an item's name a command should use for it.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Keywords {
    /// Words that are never the keyword, such as articles.
    #[serde(default)]
    pub skip: Vec<String>,
    /// Words that end the phrase holding the keyword: "of" in "a bottle of
    /// water".
    #[serde(default)]
    pub phrase_ends: Vec<String>,
    /// Whether the game takes a possessive; if not, one is never chosen
    /// while there's another word.
    #[serde(default = "takes_possessives")]
    pub possessives: bool,
}

fn takes_possessives() -> bool {
    true
}

impl Keywords {
    /// The word a command should name an item by: the last word of the
    /// phrase its name starts with. "a wooden shield" gives "shield", and
    /// with "of" ending a phrase, "a bottle of water" gives "bottle". If the
    /// game takes no possessives and that's all there is, the name they're
    /// made of.
    pub fn word_for(&self, name: &str) -> Option<String> {
        let lowercase = name.to_lowercase();
        let mut words: Vec<&str> = Vec::new();
        for word in lowercase
            .split(|c: char| !c.is_alphabetic() && c != '\'')
            .filter(|word| !word.is_empty())
        {
            if !words.is_empty() && self.phrase_ends.iter().any(|end| end == word) {
                break;
            }
            if !self.skip.iter().any(|skipped| skipped == word) {
                words.push(word);
            }
        }
        let usable = |word: &&&str| self.possessives || !word.contains('\'');
        let keyword = words.iter().rev().find(usable).copied().or_else(|| {
            let last = words.last()?;
            Some(last.split('\'').next().unwrap_or(last))
        });
        keyword.map(str::to_owned)
    }
}

impl Default for Keywords {
    fn default() -> Keywords {
        Keywords {
            skip: Vec::new(),
            phrase_ends: Vec::new(),
            possessives: takes_possessives(),
        }
    }
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

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Text {
    /// Only one applies: the highest layer's.
    pub pager: Option<Pager>,
    /// The prompt's shape, for servers that send no GA. Text the server
    /// hasn't ended is a prompt if the shape is all of it; a finished line
    /// starts with one if the shape matches at its start, output glued after
    /// it or not.
    #[serde(default, rename = "prompt")]
    pub prompts: Vec<PromptHint>,
    /// Patterns over each finished line, trigger packs included.
    #[serde(default, rename = "pattern")]
    pub patterns: Vec<EventRule>,
    /// Coloring for lines the server sends without any.
    #[serde(default, rename = "color")]
    pub colors: Vec<ColorRule>,
    /// Links and actions in lines.
    #[serde(default, rename = "link")]
    pub links: Vec<LinkRule>,
    /// GMCP messages that are lines of the chat, such as a game's channels.
    #[serde(default, rename = "channel")]
    pub channels: Vec<Channel>,
}

/// A GMCP message whose text is a line of the chat. The Text point's
/// patterns, the colors and links, and the player's triggers read it as a
/// finished line. The other points' rules, table readers, the map, gear, the
/// login and the quiet reads don't: it may be another player's words.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Channel {
    /// The field holding the line's text, perhaps colored with ANSI codes.
    pub message: GmcpField,
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
        Style {
            fg: self.fg.map(Color),
            bg: self.bg.map(Color),
            bold: self.bold,
        }
    }
}

/// Makes part of a line a link: a command to send or a URL to open.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkRule {
    pub pattern: Pattern,
    /// The action, each `{capture}` standing for its text: `"examine {item}"`.
    pub action: String,
    /// Links only this named capture, not the whole match.
    pub group: Option<String>,
}

impl LinkRule {
    /// Adds a link for each match in `text`.
    pub fn find(&self, text: &str, links: &mut Vec<Link>) {
        let group = self
            .group
            .as_deref()
            .and_then(|name| self.pattern.group(name))
            .unwrap_or(0);
        for found in self.pattern.regex.captures_iter(text) {
            let Some(span) = found.get_group(group) else {
                continue;
            };
            let action =
                self.pattern
                    .names
                    .iter()
                    .fold(self.action.clone(), |action, (name, index)| {
                        let value = found
                            .get_group(*index)
                            .map_or("", |span| &text[span.range()]);
                        action.replace(&format!("{{{name}}}"), value)
                    });
            links.push(Link {
                start: span.start as u32,
                end: span.end as u32,
                action: action.into(),
            });
        }
    }
}

impl Text {
    pub(crate) fn validate(&self) -> Result<(), String> {
        for color in &self.colors {
            if color.style().is_plain() {
                return Err(format!(
                    "the color rule `{}` sets no fg, bg or bold",
                    color.pattern.as_str()
                ));
            }
            check_group("color", &color.pattern, color.group.as_deref())?;
        }
        for link in &self.links {
            check_group("link", &link.pattern, link.group.as_deref())?;
        }
        Ok(())
    }
}

fn check_group(rule: &str, pattern: &Pattern, group: Option<&str>) -> Result<(), String> {
    match group {
        Some(group) if pattern.group(group).is_none() => Err(format!(
            "the {rule} rule `{}` names the group `{group}`, which the pattern doesn't capture",
            pattern.as_str()
        )),
        _ => Ok(()),
    }
}

// ---- HUD ----

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hud {
    /// Each vital and the GMCP field that holds it: `hp = "Char.Vitals.hp"`.
    #[serde(default)]
    pub vitals: BTreeMap<String, String>,
    /// Each character sheet key and the GMCP field that holds it, the same
    /// way: `name = "Char.Name.name"`, `str = "Char.Stats.str"`.
    #[serde(default)]
    pub character: BTreeMap<String, String>,
    /// The opponent bar's fields, the same way.
    #[serde(default)]
    pub opponent: BTreeMap<OpponentField, String>,
    /// The GMCP message that lists affects.
    pub affects: Option<String>,
}

/// What the opponent bar shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpponentField {
    /// Empty when the fight is over.
    Name,
    Level,
    Health,
    /// What `health` is out of.
    MaxHealth,
    /// Health as a percentage, for a game that sends no maximum.
    Percent,
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
    /// Reads Room.Info's zone where `areas` doesn't name it: the area
    /// captured as `name`, its level range as `low` and `high`. Only one
    /// applies: the highest layer's.
    pub area_name: Option<Pattern>,
    #[serde(default)]
    pub exits: Vec<EventRule>,
    #[serde(default)]
    pub failed_move: Vec<EventRule>,
    #[serde(default)]
    pub darkness: Vec<EventRule>,
    #[serde(default)]
    pub flee: Vec<EventRule>,
    /// Rooms whose exits don't hold still.
    #[serde(default, rename = "maze")]
    pub mazes: Vec<Maze>,
    /// The field listing the room's exits, as Room.Info's do, when a door
    /// changes in place. Only one applies: the highest layer's.
    pub room_exits: Option<GmcpField>,
    /// The message naming an area. Only one applies: the highest layer's.
    pub area_info: Option<AreaInfo>,
}

/// A GMCP message naming an area: its zone id, as Room.Info's zone gives
/// it, and its name. Both fields are in the one message.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AreaInfo {
    pub zone: GmcpField,
    pub name: GmcpField,
}

impl Map {
    pub(crate) fn validate(&self) -> Result<(), String> {
        match &self.area_info {
            Some(AreaInfo { zone, name }) if !zone.is_in(&name.package) => Err(format!(
                "`area_info`'s fields must be in one message: `zone` is in {}, `name` in {}",
                zone.package, name.package
            )),
            _ => Ok(()),
        }
    }
}

/// A maze: the rooms of an area with these names, or with none listed, the
/// whole area.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Maze {
    pub area: String,
    #[serde(default)]
    pub rooms: Vec<String>,
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
///
/// It goes out when the character enters the game, and again when a line
/// says its table is stale. Its reply stays out of the chat, line by line, so:
///
/// - The reader's `start`, `row` and `end` patterns must match every line of
///   the reply that should hide, ruled lines and headings too. Any other line
///   shows and fires triggers, as text the game sent meanwhile would. A
///   hook is free to ignore a row that holds no facts, and should do so
///   cheaply: every row counts against its budget.
/// - Name a rule of any point `<reader>.stale` to ask for the command again
///   when its line appears: one rule per reader, each with the same pattern if
///   one line makes several stale. It needs no hook.
/// - Name a rule `<reader>.none` if the game may answer with one line and no
///   table. That line and the prompt after it hide too.
///
/// Neither name changes the rule's shape: the rule is matched and delivered
/// as usual, and a plugin that doesn't use the names loses nothing.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackgroundCommand {
    pub command: String,
    pub reader: String,
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
    /// Turns a room title into a mob name, captured as `name`. The map names
    /// mobs by it, so it reads titles, not lines; the highest layer's rule
    /// that matches wins, and it can't name a hook.
    #[serde(default)]
    pub room_title: Vec<EventRule>,
}

impl Mobs {
    pub(crate) fn validate(&self) -> Result<(), String> {
        match self.room_title.iter().find(|rule| rule.hook.is_some()) {
            Some(rule) => Err(format!(
                "the room title rule `{}` can't name a hook: it reads titles, not lines",
                rule.name.as_str()
            )),
            None => Ok(()),
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn named(captures: &[Capture]) -> Vec<(&str, u32, u32)> {
        captures
            .iter()
            .map(|capture| (&*capture.name, capture.start, capture.end))
            .collect()
    }

    #[test]
    fn captures_are_the_named_groups_that_took_part() {
        let pattern =
            Pattern::new(r"^(?<who>\w+) (?:says|asks)(?<loud>!)? '(?<what>.*)'$").unwrap();
        assert_eq!(
            named(&pattern.captures("Ana says 'hi'").unwrap()),
            [("who", 0, 3), ("what", 10, 12)]
        );
        assert_eq!(
            named(&pattern.captures("Bo asks! 'why'").unwrap())[1],
            ("loud", 7, 8)
        );
        assert_eq!(pattern.captures("nothing"), None);
        assert_eq!(
            Pattern::new("plain")
                .unwrap()
                .captures("a plain line")
                .as_deref(),
            Some(&[][..])
        );
    }

    #[test]
    fn spans_cover_every_match_of_a_group() {
        let pattern = Pattern::new(r"(?<n>\d+)(?<unit>h)?").unwrap();
        let unit = pattern.group("unit").unwrap();
        assert_eq!(
            pattern.spans("10h 20m 30h", 0).collect::<Vec<_>>(),
            [(0, 3), (4, 6), (8, 11)]
        );
        assert_eq!(
            pattern.spans("10h 20m 30h", unit).collect::<Vec<_>>(),
            [(2, 3), (10, 11)]
        );
        assert_eq!(pattern.group("missing"), None);
    }

    #[test]
    fn a_fragment_expands_as_one_group_and_nothing_else_does() {
        let fragments = BTreeMap::from([("tag".to_owned(), "a|b".to_owned())]);
        assert_eq!(expand(r"^{tag}?x{2,}$", &fragments), r"^(?:a|b)?x{2,}$");
        assert_eq!(expand(r"\{tag}", &fragments), r"\{tag}");
        assert_eq!(expand(r"\\{tag}", &fragments), r"\\(?:a|b)");
        assert_eq!(expand(r"{other}", &fragments), r"{other}");
        assert_eq!(expand("{", &fragments), "{");
    }

    #[test]
    fn a_link_rule_fills_its_action_from_the_captures() {
        let rule = LinkRule {
            pattern: Pattern::new(r"take (?<item>\w+)(?<rest> now)?").unwrap(),
            action: "get {item}{rest}".to_owned(),
            group: Some("item".to_owned()),
        };
        let mut links = Vec::new();
        rule.find("take sword, take shield now", &mut links);
        assert_eq!(
            links,
            [
                Link {
                    start: 5,
                    end: 10,
                    action: "get sword".into()
                },
                Link {
                    start: 17,
                    end: 23,
                    action: "get shield now".into()
                }
            ]
        );
    }

    #[test]
    fn a_pattern_that_does_not_compile_says_why() {
        let error = Pattern::new("(unclosed").unwrap_err();
        assert!(error.to_string().contains("unclosed group"), "{error}");
    }
}
