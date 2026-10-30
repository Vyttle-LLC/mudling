//! The check before every push: nothing but `secrets.vault` holds a password
//! (PRD §10).
//!
//! It looks for what a password looks like from outside, because a locked
//! vault can't say what the passwords are: a field named for one, in any JSON
//! file and in JSON held inside a string (`Server.settings`); `password: x`
//! in text the player wrote; and a trigger that answers a password prompt.
//! Game text doesn't count, because a room may well say "password: mellon".
//! A password in an alias with nothing around it to tell it apart isn't found.

use crate::repo::format::{CHARACTERS_FOLDER, GEAR_FOLDER, MAP_FOLDER, MOBS, NOTES, SETTINGS};
use crate::vault;
use regex::Regex;
use serde_json::{Map, Value};
use std::path::Path;
use std::sync::LazyLock;

const PASSWORD_WORD: &str = r"pass(word|wd|phrase|code)|pwd";

/// The name of a field that holds a secret.
static SECRET_FIELD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?i){PASSWORD_WORD}|^pass$|secret|api_?key|token$"
    ))
    .expect("a valid pattern")
});

/// A setting or alias that's named for a password.
static PASSWORD_NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!("(?i){PASSWORD_WORD}")).expect("a valid pattern"));

/// `password: hunter2` or `pwd=hunter2`.
static SPOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(r"(?i)\b({PASSWORD_WORD})\s*[:=]\s*\S")).expect("a valid pattern")
});

/// `password = "hunter2"` in a plugin's source, where a bare value is more
/// often a variable.
static QUOTED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(r#"(?i)\b({PASSWORD_WORD})\s*[:=]\s*["'][^"']"#)).expect("a valid pattern")
});

/// A trigger's pattern for a password prompt.
static PROMPT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"(?i)({PASSWORD_WORD})\s*:")).expect("a valid pattern"));

/// Where `bytes`, a file at `path` in the repo, seems to hold a password. The
/// answer names the field or line and never quotes the value.
pub fn find_password(path: &Path, bytes: &[u8]) -> Option<String> {
    if path == Path::new(vault::FILE_NAME) {
        return None;
    }
    let text = std::str::from_utf8(bytes).ok()?;
    if let Ok(value) = serde_json::from_str::<Value>(text) {
        let scan = Scan {
            authored: !is_game_text(path),
            named_fields: path != Path::new(SETTINGS),
        };
        return scan.value(&value, "");
    }
    let pattern = if path.file_name().is_some_and(|name| name == NOTES) {
        &SPOKEN
    } else {
        &QUOTED
    };
    let line = text.lines().position(|line| pattern.is_match(line))?;
    Some(format!("line {}", line + 1))
}

/// Whether the game wrote this file's text, not the player: a server folder's
/// map, characters, gear and mobs.
fn is_game_text(path: &Path) -> bool {
    path.iter().nth(1).is_some_and(|part| {
        part == MAP_FOLDER || part == CHARACTERS_FOLDER || part == GEAR_FOLDER || part == MOBS
    })
}

#[derive(Clone, Copy)]
struct Scan {
    /// The player wrote the values, so they mean what they say.
    authored: bool,
    /// Field names are the player's, not a list like `settings.json`'s
    /// shortcuts, whose names are app actions.
    named_fields: bool,
}

impl Scan {
    fn value(self, value: &Value, field: &str) -> Option<String> {
        match value {
            Value::Object(object) => self.object(object, field),
            Value::Array(items) => items
                .iter()
                .enumerate()
                .find_map(|(index, item)| self.value(item, &format!("{field}[{index}]"))),
            Value::String(text) => self.string(text, field),
            _ => None,
        }
    }

    fn object(self, object: &Map<String, Value>, field: &str) -> Option<String> {
        if self.authored && (is_named_password(object) || answers_a_prompt(object)) {
            return Some(field.to_owned());
        }
        object.iter().find_map(|(name, value)| {
            let inner = if field.is_empty() {
                name.clone()
            } else {
                format!("{field}.{name}")
            };
            if self.named_fields && SECRET_FIELD.is_match(name) && has_text(value) {
                return Some(inner);
            }
            // A pattern can mention a password without holding one.
            let scan = Scan {
                authored: self.authored && name != "matches",
                ..self
            };
            scan.value(value, &inner)
        })
    }

    /// Text may be JSON itself, as `Server.settings` is.
    fn string(self, text: &str, field: &str) -> Option<String> {
        let nested = serde_json::from_str::<Value>(text).ok();
        if let Some(value) = nested.filter(|value| value.is_object() || value.is_array()) {
            return self.value(&value, field);
        }
        (self.authored && SPOKEN.is_match(text)).then(|| field.to_owned())
    }
}

/// A key and value, or an alias and what it expands to, named for a password.
fn is_named_password(object: &Map<String, Value>) -> bool {
    let name = text_field(object, "key").or_else(|| text_field(object, "name"));
    let held = text_field(object, "value").or_else(|| text_field(object, "expansion"));
    name.is_some_and(|name| PASSWORD_NAME.is_match(name))
        && held.is_some_and(|held| !held.is_empty())
}

fn answers_a_prompt(object: &Map<String, Value>) -> bool {
    text_field(object, "matches").is_some_and(|matches| PROMPT.is_match(matches))
        && text_field(object, "action").is_some_and(|action| !action.is_empty())
}

fn text_field<'a>(object: &'a Map<String, Value>, name: &str) -> Option<&'a str> {
    object.get(name).and_then(Value::as_str)
}

fn has_text(value: &Value) -> bool {
    value.as_str().is_some_and(|text| !text.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(path: &str, text: &str) -> Option<String> {
        let path: std::path::PathBuf = path.split('/').collect();
        find_password(&path, text.as_bytes())
    }

    #[test]
    fn a_password_in_server_settings_is_found_inside_its_json_string() {
        let settings = r#"{\"encoding\":\"utf-8\",\"login\":{\"password\":\"hunter2\"}}"#;
        let list = format!(r#"{{"servers":[{{"id":"a","settings":"{settings}"}}]}}"#);

        let field = found("servers.json", &list).unwrap();

        assert_eq!(field, "servers[0].settings.login.password");
        assert!(!field.contains("hunter2"));
    }

    #[test]
    fn an_empty_field_or_a_flag_holds_no_password() {
        let settings = r#"{\"password\":\"\",\"save_password\":true}"#;
        let list = format!(r#"{{"servers":[{{"id":"a","settings":"{settings}"}}]}}"#);

        assert_eq!(found("servers.json", &list), None);
    }

    #[test]
    fn a_saved_prompt_answer_is_found_but_a_trigger_that_only_mentions_one_is_not() {
        let answers = r#"{"triggers":[{"matches":"^Password:","action":"hunter2"}]}"#;
        let mentions = r#"{"triggers":[{"matches":"wrong password","action":"flee"}]}"#;

        assert!(found("mud.example.org/triggers.json", answers).is_some());
        assert_eq!(found("mud.example.org/triggers.json", mentions), None);
    }

    #[test]
    fn an_alias_named_for_a_password_or_spelling_one_out_is_found() {
        let named = r#"{"aliases":[{"name":"mypassword","expansion":"hunter2"}]}"#;
        let spelled = r#"{"aliases":[{"name":"in","expansion":"login password=hunter2"}]}"#;
        let harmless = r#"{"aliases":[{"name":"pass","expansion":"cast 'pass door'"}]}"#;

        assert!(found("mud.example.org/aliases.json", named).is_some());
        assert!(found("mud.example.org/aliases.json", spelled).is_some());
        assert_eq!(found("mud.example.org/aliases.json", harmless), None);
    }

    #[test]
    fn notes_are_read_line_by_line() {
        let notes = "Buy rope.\nguild password: mellon\n";

        assert_eq!(
            found("mud.example.org/notes.txt", notes),
            Some("line 2".to_owned())
        );
        assert_eq!(
            found("mud.example.org/notes.txt", "Forgot my password"),
            None
        );
    }

    #[test]
    fn game_text_and_the_vault_are_not_read_as_passwords() {
        let room = r#"{"rooms":[{"name":"A gate","description":"password: mellon"}]}"#;

        assert_eq!(found("mud.example.org/map/Moria.json", room), None);
        assert_eq!(found("secrets.vault", r#"{"password":"sealed"}"#), None);
    }

    #[test]
    fn a_shortcut_named_for_the_vault_is_not_a_password() {
        let settings = r#"{"shortcuts":{"vault.reveal_password":"cmd+r"}}"#;

        assert_eq!(found("settings.json", settings), None);
    }

    #[test]
    fn a_plugin_may_name_a_password_but_not_hold_one() {
        assert_eq!(
            found("plugins/login.luau", "local password = vault.get()"),
            None
        );
        assert!(found("plugins/login.toml", "password = \"hunter2\"\n").is_some());
    }
}
