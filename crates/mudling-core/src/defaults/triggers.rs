//! The player's triggers: a line that matches is highlighted, hidden or rings
//! a sound. Triggers never send commands; acting stays the player's.
//!
//! Their patterns join the plugins' in the host's merged matcher
//! ([`crate::plugin::PluginHost::set_triggers`]), so they cost one pass over
//! the line, however many there are. Here is what they are, how a row holds
//! one, and what the ones matching a line ask for together.

use crate::ansi::{Color, Style};
use crate::persist::{self, Timestamp};
use crate::plugin::points::Pattern;
use serde::{Deserialize, Serialize};

const ALL_SCOPE: &str = "all";
const SERVER_SCOPE: &str = "server";
const CHARACTER_SCOPE: &str = "character:";

/// Who a trigger or alias applies to.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Scope {
    /// Every character, on every server.
    All,
    /// Every character on the server whose database keeps it.
    #[default]
    Server,
    /// One character, by id.
    Character(String),
}

impl Scope {
    /// Reads a row's `scope`. A value this version doesn't know stays with
    /// its server, rather than reach characters it wasn't meant for.
    pub(crate) fn from_row(row: &str) -> Scope {
        match row.strip_prefix(CHARACTER_SCOPE) {
            Some(character) => Scope::Character(character.to_owned()),
            None if row == ALL_SCOPE => Scope::All,
            None => Scope::Server,
        }
    }

    pub(crate) fn to_row(&self) -> String {
        match self {
            Scope::All => ALL_SCOPE.to_owned(),
            Scope::Server => SERVER_SCOPE.to_owned(),
            Scope::Character(character) => format!("{CHARACTER_SCOPE}{character}"),
        }
    }

    /// Whether it applies to `character` on the server this session plays.
    pub(crate) fn covers(&self, character: &str) -> bool {
        match self {
            Scope::Character(id) => id == character,
            Scope::All | Scope::Server => true,
        }
    }
}

/// A trigger's color, as the prototype named them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Highlight {
    Red,
    Orange,
    Yellow,
    Green,
    Teal,
    Blue,
    Purple,
    Pink,
}

impl Highlight {
    pub const ALL: [Highlight; 8] = [
        Highlight::Red,
        Highlight::Orange,
        Highlight::Yellow,
        Highlight::Green,
        Highlight::Teal,
        Highlight::Blue,
        Highlight::Purple,
        Highlight::Pink,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Highlight::Red => "Red",
            Highlight::Orange => "Orange",
            Highlight::Yellow => "Yellow",
            Highlight::Green => "Green",
            Highlight::Teal => "Teal",
            Highlight::Blue => "Blue",
            Highlight::Purple => "Purple",
            Highlight::Pink => "Pink",
        }
    }

    /// How the line is drawn, in the 256-color palette, which every theme
    /// shows the same.
    pub fn style(self) -> Style {
        let index = match self {
            Highlight::Red => 203,
            Highlight::Orange => 208,
            Highlight::Yellow => 178,
            Highlight::Green => 71,
            Highlight::Teal => 37,
            Highlight::Blue => 69,
            Highlight::Purple => 134,
            Highlight::Pink => 205,
        };
        Style {
            fg: Some(Color(index)),
            ..Style::default()
        }
    }
}

/// What a row's `action` column holds.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct TriggerAction {
    regex: bool,
    color: Option<Highlight>,
    hide: bool,
    sound: Option<String>,
}

/// One trigger, as the player edits it.
#[derive(Clone, Debug, PartialEq)]
pub struct TriggerRule {
    /// Empty until the session first saves it.
    pub id: String,
    pub scope: Scope,
    /// Text the line contains, or a regular expression; either ignores case.
    pub pattern: String,
    pub regex: bool,
    pub color: Option<Highlight>,
    /// Keeps the line out of the chat; its sound still plays.
    pub hides: bool,
    /// A bundled sound's name.
    pub sound: Option<String>,
    pub enabled: bool,
    /// The server whose database keeps it.
    pub home: String,
}

impl TriggerRule {
    pub fn new(pattern: impl Into<String>) -> TriggerRule {
        TriggerRule {
            id: String::new(),
            scope: Scope::default(),
            pattern: pattern.into(),
            regex: false,
            color: None,
            hides: false,
            sound: None,
            enabled: true,
            home: String::new(),
        }
    }

    /// Why the pattern can't match anything, if it can't.
    pub fn problem(&self) -> Option<&'static str> {
        if self.pattern.is_empty() {
            return Some("Type the text to match.");
        }
        self.compiled()
            .is_none()
            .then_some("That isn't a valid regular expression.")
    }

    /// The pattern as the matcher takes it, or `None` if it won't compile.
    pub(crate) fn compiled(&self) -> Option<Pattern> {
        let source = if self.regex {
            format!("(?i){}", self.pattern)
        } else {
            format!("(?i){}", regex::escape(&self.pattern))
        };
        Pattern::new(&source).ok()
    }

    pub(crate) fn from_row(row: persist::Trigger, home: &str) -> TriggerRule {
        let action: TriggerAction = serde_json::from_str(&row.action).unwrap_or_default();
        TriggerRule {
            scope: Scope::from_row(&row.scope),
            regex: action.regex,
            color: action.color,
            hides: action.hide,
            sound: action.sound,
            enabled: row.enabled,
            home: home.to_owned(),
            pattern: row.matches,
            id: row.id,
        }
    }

    pub(crate) fn to_row(&self, updated: Timestamp) -> persist::Trigger {
        let action = TriggerAction {
            regex: self.regex,
            color: self.color,
            hide: self.hides,
            sound: self.sound.clone(),
        };
        persist::Trigger {
            id: self.id.clone(),
            scope: self.scope.to_row(),
            matches: self.pattern.clone(),
            action: serde_json::to_string(&action).expect("the action is plain data"),
            enabled: self.enabled,
            updated,
        }
    }
}

/// What the triggers matching one line ask for together: the first color,
/// hidden if any hides it, and each sound once.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TriggerResponse {
    pub color: Option<Highlight>,
    pub hides: bool,
    pub sounds: Vec<String>,
}

impl TriggerResponse {
    /// `matched` are indexes into `rules`, in order.
    pub fn to(matched: &[usize], rules: &[TriggerRule]) -> TriggerResponse {
        let mut response = TriggerResponse::default();
        for rule in matched.iter().map(|&index| &rules[index]) {
            response.color = response.color.or(rule.color);
            response.hides |= rule.hides;
            if let Some(sound) = rule.sound.as_ref()
                && !response.sounds.contains(sound)
            {
                response.sounds.push(sound.clone());
            }
        }
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_row_keeps_everything_the_player_set() {
        let rule = TriggerRule {
            id: "t1".to_owned(),
            scope: Scope::Character("ana".to_owned()),
            regex: true,
            color: Some(Highlight::Purple),
            hides: true,
            sound: Some("chime".to_owned()),
            enabled: false,
            home: "mud.example".to_owned(),
            ..TriggerRule::new("^tells")
        };
        let row = rule.to_row(Timestamp(1));
        assert_eq!(row.scope, "character:ana");
        assert_eq!(TriggerRule::from_row(row, "mud.example"), rule);
    }

    #[test]
    fn a_row_from_elsewhere_with_another_action_is_a_plain_trigger() {
        let row = persist::Trigger {
            id: "t1".to_owned(),
            scope: "server".to_owned(),
            matches: "^Der Dachs".to_owned(),
            action: "flee".to_owned(),
            enabled: true,
            updated: Timestamp(1),
        };
        let rule = TriggerRule::from_row(row, "mud.example");
        assert_eq!((rule.color, rule.hides), (None, false));
    }

    #[test]
    fn a_scope_this_version_does_not_know_stays_with_its_server() {
        assert_eq!(Scope::from_row("all"), Scope::All);
        assert_eq!(Scope::from_row("guild:rangers"), Scope::Server);
    }

    #[test]
    fn a_character_scope_covers_that_character_only() {
        let scope = Scope::Character("ana".to_owned());
        assert!(scope.covers("ana"));
        assert!(!scope.covers("bo"));
        assert!(Scope::Server.covers("bo"));
    }

    #[test]
    fn matches_combine() {
        let dragon = TriggerRule {
            color: Some(Highlight::Red),
            sound: Some("low".to_owned()),
            ..TriggerRule::new("dragon")
        };
        let red_dragon = TriggerRule {
            color: Some(Highlight::Yellow),
            sound: Some("low".to_owned()),
            ..TriggerRule::new("red dragon")
        };
        let red = TriggerRule {
            hides: true,
            ..TriggerRule::new("red")
        };
        let response = TriggerResponse::to(&[0, 1, 2], &[dragon, red_dragon, red]);
        assert_eq!(
            response,
            TriggerResponse {
                color: Some(Highlight::Red),
                hides: true,
                sounds: vec!["low".to_owned()],
            }
        );
    }

    #[test]
    fn a_bad_regex_has_a_problem_and_an_empty_pattern_too() {
        let broken = TriggerRule {
            regex: true,
            ..TriggerRule::new("rat(")
        };
        assert!(broken.problem().is_some());
        assert!(TriggerRule::new("").problem().is_some());
        assert!(TriggerRule::new("rat(").problem().is_none(), "plain text");
    }
}
