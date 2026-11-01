//! The character sheet: what plugins read about one character, as typed rows.
//!
//! Hooks write generic `<store>.<key>` values. The ones with a typed home go
//! there, by these shapes, which every game plugin shares:
//!
//! - `skills`: `<skill>.learned`, `<skill>.level` and `<skill>.mana` are a
//!   [`persist::Skill`]; `practice.sessions` is what's left to spend.
//! - `affects`: `<n>.name` and `<n>.detail`, then `count`, which replaces the
//!   whole list of [`persist::Affect`]s.
//! - `character`: the rest are stats. `<reader>.read`, here or in `skills`,
//!   says a table finished.
//!
//! Everything else stays a [`persist::CharacterStat`].
//!
//! The sheet belongs to the character this connection named. Their rows load
//! when that happens, and no other character's ever do.

use crate::persist::{self, Batch, CharacterRows, Key, Timestamp};
use crate::plugin::luau::Value;
use crate::store::{Affect, Improved, SheetState, SkillRow};
use std::collections::BTreeMap;

const CHARACTER: &str = "character";
const SKILLS: &str = "skills";
const AFFECTS: &str = "affects";
const LEARNED: &str = "learned";
const LEVEL: &str = "level";
const MANA: &str = "mana";
const SESSIONS: &str = "practice.sessions";
/// The same count, as the game keeps it current on the `character` side.
const LIVE_PRACTICES: &str = "practice";
const TABLE_READ: &str = "read";
const COUNT: &str = "count";
const NAME: &str = "name";
const DETAIL: &str = "detail";
/// An affect the game lists twice, with two details, is one row.
const DETAILS_SEPARATOR: &str = "\n";

/// One character's sheet, and what it took to make it.
#[derive(Debug, Default)]
pub(super) struct Sheet {
    state: SheetState,
    /// The readers that fed each of the sheet's parts.
    readers: Vec<String>,
    /// An `affects` read on its way, by row.
    arriving_affects: BTreeMap<u32, (String, String)>,
    changed: bool,
}

impl Sheet {
    /// The sheet of `character`, from what's saved.
    pub(super) fn of(character: &str, rows: &CharacterRows) -> Sheet {
        let mut sheet = Sheet {
            state: SheetState {
                character: character.to_owned(),
                ..SheetState::default()
            },
            changed: true,
            ..Sheet::default()
        };
        let state = &mut sheet.state;
        let mine = |id: &str| id == character;
        let mut practices_at = None;
        let mut practices = |state: &mut SheetState, value: &Value, at: Timestamp| {
            if practices_at < Some(at) {
                practices_at = Some(at);
                state.practices = whole(value);
            }
        };
        for stat in rows.stats.iter().filter(|stat| mine(&stat.character_id)) {
            let Some(value) = super::values::from_row(&stat.value) else {
                continue;
            };
            let Some((store, key)) = stat.key.split_once('.') else {
                continue;
            };
            match (store, key.rsplit_once('.')) {
                (SKILLS, _) if key == SESSIONS => practices(state, &value, stat.updated),
                (SKILLS, Some((_, TABLE_READ))) => {
                    state.skills_read = state.skills_read.max(Some(stat.updated));
                }
                (CHARACTER, Some((reader, TABLE_READ))) => {
                    sheet.readers.push(reader.to_owned());
                    state.character_read = state.character_read.max(Some(stat.updated));
                }
                (AFFECTS, None) if key == COUNT => {
                    sheet.readers.push(AFFECTS.to_owned());
                    state.character_read = state.character_read.max(Some(stat.updated));
                }
                (CHARACTER, _) => {
                    if key == LIVE_PRACTICES {
                        practices(state, &value, stat.updated);
                    }
                    state.stats.insert(key.to_owned(), value);
                }
                _ => {}
            }
        }
        state.skills = (rows.skills.iter())
            .filter(|skill| mine(&skill.character_id))
            .map(|skill| SkillRow {
                name: skill.name.clone(),
                level: skill.level,
                learned: skill.learned,
                mana: skill.cost,
            })
            .collect();
        state.affects = (rows.affects.iter())
            .filter(|affect| mine(&affect.character_id))
            .map(|affect| Affect {
                name: affect.name.clone(),
                detail: affect.detail.clone().unwrap_or_default(),
            })
            .collect();
        sheet
    }

    pub(super) fn character(&self) -> &str {
        &self.state.character
    }

    pub(super) fn set_reading(&mut self, reading: bool) {
        self.changed |= self.state.reading != reading;
        self.state.reading = reading;
    }

    /// Takes in a value a plugin wrote or the game sent, adding any row it
    /// makes to `batch`. Returns whether a typed row took it: otherwise it's
    /// the caller's to save as a stat.
    pub(super) fn keep(
        &mut self,
        (store, key): (&str, &str),
        value: &Value,
        now: Timestamp,
        batch: &mut Batch,
    ) -> bool {
        match store {
            SKILLS => self.keep_skill(key, value, now, batch),
            AFFECTS => self.keep_affect(key, value, now, batch),
            CHARACTER => {
                self.changed = true;
                match key.rsplit_once('.') {
                    Some((reader, TABLE_READ)) => {
                        self.note_reader(reader);
                        self.state.character_read = Some(now);
                    }
                    _ => {
                        if key == LIVE_PRACTICES {
                            self.state.practices = whole(value);
                        }
                        self.state.stats.insert(key.to_owned(), value.clone());
                    }
                }
                false
            }
            _ => false,
        }
    }

    /// What changed since the last call, for the store: `command_of` gives
    /// the command behind a reader.
    pub(super) fn take_change<'a>(
        &mut self,
        command_of: impl Fn(&str) -> Option<&'a str>,
    ) -> Option<SheetState> {
        if !std::mem::take(&mut self.changed) {
            return None;
        }
        self.state.sources = (self.readers.iter())
            .map(|reader| command_of(reader).unwrap_or(reader).to_owned())
            .collect();
        Some(self.state.clone())
    }

    fn note_reader(&mut self, reader: &str) {
        if !self.readers.iter().any(|known| known == reader) {
            self.readers.push(reader.to_owned());
        }
    }

    fn keep_skill(&mut self, key: &str, value: &Value, now: Timestamp, batch: &mut Batch) -> bool {
        if key == SESSIONS {
            self.state.practices = whole(value);
            self.changed = true;
            return false;
        }
        let Some((name, field)) = key.rsplit_once('.') else {
            return false;
        };
        if field == TABLE_READ {
            self.state.skills_read = Some(now);
            self.changed = true;
            return false;
        }
        if ![LEARNED, LEVEL, MANA].contains(&field) {
            return false;
        }
        let Some(number) = whole(value) else {
            return false;
        };
        let skills = &mut self.state.skills;
        let at = match skills.binary_search_by(|skill| skill.name.as_str().cmp(name)) {
            Ok(at) => at,
            Err(at) => {
                skills.insert(at, SkillRow::named(name));
                at
            }
        };
        let skill = &mut skills[at];
        match field {
            LEARNED => {
                if skill.learned.is_some_and(|learned| learned < number) {
                    self.state.improved = Some(Improved {
                        skill: name.to_owned(),
                        learned: number,
                        at: now,
                    });
                }
                skill.learned = Some(number);
            }
            LEVEL => skill.level = Some(number),
            _ => skill.mana = Some(number),
        }
        batch.upsert(persist::Skill {
            character_id: self.state.character.clone(),
            name: name.to_owned(),
            level: skill.level,
            learned: skill.learned,
            cost: skill.mana,
            updated: now,
        });
        self.changed = true;
        true
    }

    fn keep_affect(&mut self, key: &str, value: &Value, now: Timestamp, batch: &mut Batch) -> bool {
        if key == COUNT {
            self.replace_affects(now, batch);
            return false;
        }
        let (Some((row, field)), Value::Text(text)) = (key.split_once('.'), value) else {
            return false;
        };
        let Ok(row) = row.parse() else { return false };
        let arriving = self.arriving_affects.entry(row).or_default();
        match field {
            NAME => arriving.0.clone_from(text),
            DETAIL => arriving.1.clone_from(text),
            _ => return false,
        }
        true
    }

    /// The table finished: what arrived is the whole list.
    fn replace_affects(&mut self, now: Timestamp, batch: &mut Batch) {
        let mut affects: Vec<Affect> = Vec::new();
        for (name, detail) in std::mem::take(&mut self.arriving_affects).into_values() {
            match affects.iter_mut().find(|affect| affect.name == name) {
                Some(affect) => {
                    affect.detail.push_str(DETAILS_SEPARATOR);
                    affect.detail.push_str(&detail);
                }
                None => affects.push(Affect { name, detail }),
            }
        }
        let character = &self.state.character;
        for gone in (self.state.affects.iter())
            .filter(|old| !affects.iter().any(|affect| affect.name == old.name))
        {
            batch.delete(Key::Affect {
                character_id: character.clone(),
                name: gone.name.clone(),
            });
        }
        for affect in &affects {
            batch.upsert(persist::Affect {
                character_id: character.clone(),
                name: affect.name.clone(),
                detail: Some(affect.detail.clone()).filter(|detail| !detail.is_empty()),
                updated: now,
            });
        }
        self.state.affects = affects;
        self.note_reader(AFFECTS);
        self.state.character_read = Some(now);
        self.changed = true;
    }
}

fn whole(value: &Value) -> Option<i64> {
    match value {
        Value::Number(number) => Some(*number as i64),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persist::{Affect as AffectRow, CharacterStat, Skill};

    const NOW: Timestamp = Timestamp(5);

    fn sheet() -> Sheet {
        Sheet::of("bren", &CharacterRows::default())
    }

    fn number(sheet: &mut Sheet, store: &str, key: &str, value: f64) -> bool {
        let value = Value::Number(value);
        sheet.keep((store, key), &value, NOW, &mut Batch::new())
    }

    fn text(sheet: &mut Sheet, store: &str, key: &str, value: &str, batch: &mut Batch) -> bool {
        sheet.keep((store, key), &Value::Text(value.to_owned()), NOW, batch)
    }

    #[test]
    fn a_skills_fields_arrive_apart_and_make_one_row() {
        let mut sheet = sheet();
        assert!(number(&mut sheet, "skills", "dodge.learned", 45.));
        assert!(number(&mut sheet, "skills", "dodge.level", 4.));
        assert!(number(&mut sheet, "skills", "dodge.mana", 0.));
        assert_eq!(
            sheet.state.skills,
            [SkillRow {
                name: "dodge".to_owned(),
                level: Some(4),
                learned: Some(45),
                mana: Some(0),
            }]
        );
    }

    #[test]
    fn counts_and_other_keys_stay_stats() {
        let mut sheet = sheet();
        assert!(!number(&mut sheet, "skills", "practice.sessions", 4.));
        assert!(!number(&mut sheet, "skills", "practice.read", 12.));
        assert!(!number(&mut sheet, "skills", "dodge", 12.));
        assert_eq!(sheet.state.practices, Some(4));
        assert_eq!(sheet.state.skills_read, Some(NOW));
        assert!(sheet.state.skills.is_empty());
    }

    #[test]
    fn practices_left_follows_whichever_said_it_last() {
        let mut sheet = sheet();
        number(&mut sheet, "skills", "practice.sessions", 4.);
        number(&mut sheet, "character", "practice", 3.);
        assert_eq!(sheet.state.practices, Some(3), "the live count falls");
        number(&mut sheet, "skills", "practice.sessions", 8.);
        assert_eq!(sheet.state.practices, Some(8));

        let stat = |key: &str, value: &str, updated: i64| CharacterStat {
            character_id: "bren".to_owned(),
            key: key.to_owned(),
            value: value.to_owned(),
            updated: Timestamp(updated),
        };
        let rows = CharacterRows {
            stats: vec![
                stat("character.practice", "3.0", 9),
                stat("skills.practice.sessions", "4.0", 2),
            ],
            ..CharacterRows::default()
        };
        assert_eq!(Sheet::of("bren", &rows).state.practices, Some(3));
    }

    #[test]
    fn only_a_rise_in_a_known_skill_is_an_improvement() {
        let mut sheet = sheet();
        number(&mut sheet, "skills", "dodge.learned", 45.);
        assert_eq!(sheet.state.improved, None, "the first reading");
        number(&mut sheet, "skills", "dodge.learned", 45.);
        assert_eq!(sheet.state.improved, None, "unchanged");
        number(&mut sheet, "skills", "dodge.learned", 46.);
        let improved = sheet.state.improved.clone().unwrap();
        assert_eq!((&*improved.skill, improved.learned), ("dodge", 46));
    }

    #[test]
    fn an_affects_count_replaces_the_list_and_deletes_what_wore_off() {
        let mut sheet = sheet();
        let mut batch = Batch::new();
        assert!(text(&mut sheet, "affects", "1.name", "bless", &mut batch));
        assert!(text(&mut sheet, "affects", "2.name", "armor", &mut batch));
        assert!(sheet.state.affects.is_empty(), "not until the count");
        number(&mut sheet, "affects", "count", 2.);
        assert_eq!(sheet.state.affects.len(), 2);

        text(&mut sheet, "affects", "1.name", "armor", &mut batch);
        number(&mut sheet, "affects", "count", 1.);
        let names: Vec<&str> = sheet.state.affects.iter().map(|a| &*a.name).collect();
        assert_eq!(names, ["armor"]);

        number(&mut sheet, "affects", "count", 0.);
        assert!(sheet.state.affects.is_empty(), "the last wore off");
    }

    #[test]
    fn one_affect_with_two_details_is_one_row() {
        let mut sheet = sheet();
        let mut batch = Batch::new();
        text(&mut sheet, "affects", "1.name", "bless", &mut batch);
        text(&mut sheet, "affects", "1.detail", "hit roll", &mut batch);
        text(&mut sheet, "affects", "2.name", "bless", &mut batch);
        text(&mut sheet, "affects", "2.detail", "saves", &mut batch);
        sheet.replace_affects(NOW, &mut batch);
        assert_eq!(sheet.state.affects.len(), 1);
        assert_eq!(sheet.state.affects[0].detail, "hit roll\nsaves");
    }

    #[test]
    fn a_character_loads_only_their_own_rows() {
        let stat = |character: &str, key: &str, value: &str| CharacterStat {
            character_id: character.to_owned(),
            key: key.to_owned(),
            value: value.to_owned(),
            updated: Timestamp(9),
        };
        let skill = |character: &str, name: &str| Skill {
            character_id: character.to_owned(),
            name: name.to_owned(),
            level: Some(1),
            learned: Some(10),
            cost: None,
            updated: Timestamp(9),
        };
        let affect = |character: &str, name: &str| AffectRow {
            character_id: character.to_owned(),
            name: name.to_owned(),
            detail: None,
            updated: Timestamp(9),
        };
        let rows = CharacterRows {
            stats: vec![
                stat("bren", "character.str", "14.0"),
                stat("bren", "character.score.read", "12.0"),
                stat("mira", "character.str", "9.0"),
            ],
            skills: vec![skill("bren", "kick"), skill("mira", "bash")],
            affects: vec![affect("bren", "bless"), affect("mira", "armor")],
            ..CharacterRows::default()
        };
        let mut sheet = Sheet::of("bren", &rows);
        let state = sheet.take_change(|_| None).expect("it's new");
        assert_eq!(state.stats.len(), 1);
        assert_eq!(state.stats["str"], Value::Number(14.));
        assert_eq!(state.skills.len(), 1);
        assert_eq!(state.skills[0].name, "kick");
        assert_eq!(state.affects[0].name, "bless");
        assert_eq!(state.character_read, Some(Timestamp(9)));
        assert_eq!(state.sources, ["score"]);
    }

    #[test]
    fn the_sheet_names_a_readers_command() {
        let mut sheet = sheet();
        number(&mut sheet, "character", "score.read", 8.);
        let state = sheet.take_change(|_| Some("look-at-me")).unwrap();
        assert_eq!(state.sources, ["look-at-me"]);
        assert!(sheet.take_change(|_| None).is_none(), "no change since");
    }
}
