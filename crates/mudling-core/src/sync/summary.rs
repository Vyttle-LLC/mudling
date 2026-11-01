//! A session in a line, for its squashed commit and for Data & sync:
//! "A MUD · Fikus · 2h10: 84 rooms, 3 skills, 2 identifies".

use crate::git::{self, Oid};
use crate::merge;
use crate::repo::format::ServerRows;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::Duration;

const NO_RECORDS: &str = "Changes to settings";

type Servers = BTreeMap<String, (ServerRows, PathBuf)>;

/// What changed between `from` and `to`, over `played` if that's known.
pub(super) fn session_line(
    git: &git::Repo,
    from: Oid,
    to: Oid,
    played: Option<Duration>,
) -> Result<String, crate::merge::Error> {
    let before = merge::servers(&git.files_at(from)?)?;
    let after = merge::servers(&git.files_at(to)?)?;
    Ok(describe(&before, &after, played))
}

fn describe(before: &Servers, after: &Servers, played: Option<Duration>) -> String {
    let mut lines = Vec::new();
    for (id, (rows, _)) in after {
        let earlier = before.get(id).map(|(rows, _)| rows);
        let counts = counts(earlier, rows);
        if counts.is_empty() {
            continue;
        }
        let mut heading: Vec<String> = rows.servers.iter().map(|s| s.name.clone()).collect();
        heading.extend(changed_characters(earlier, rows));
        heading.extend(played.map(in_hours));
        lines.push(format!("{}: {}", heading.join(" · "), counts.join(", ")));
    }
    if lines.is_empty() {
        NO_RECORDS.to_owned()
    } else {
        lines.join("; ")
    }
}

/// How many of each kind of record are new.
fn counts(before: Option<&ServerRows>, after: &ServerRows) -> Vec<String> {
    let rooms = new_records(
        before.map(|rows| rows.map.rooms.iter().map(|room| room.id).collect()),
        after.map.rooms.iter().map(|room| room.id),
    );
    let skill = |skill: &crate::persist::Skill| (skill.character_id.clone(), skill.name.clone());
    let skills = new_records(
        before.map(|rows| rows.characters.skills.iter().map(skill).collect()),
        after.characters.skills.iter().map(skill),
    );
    let identifies = new_records(
        before.map(|rows| {
            rows.gear
                .identifications
                .iter()
                .map(|i| i.id.clone())
                .collect()
        }),
        after.gear.identifications.iter().map(|i| i.id.clone()),
    );
    [(rooms, "room"), (skills, "skill"), (identifies, "identify")]
        .into_iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, noun)| match (count, noun) {
            (1, noun) => format!("1 {noun}"),
            (count, "identify") => format!("{count} identifies"),
            (count, noun) => format!("{count} {noun}s"),
        })
        .collect()
}

fn new_records<K: Ord>(before: Option<BTreeSet<K>>, after: impl Iterator<Item = K>) -> usize {
    let before = before.unwrap_or_default();
    after.filter(|key| !before.contains(key)).count()
}

/// The characters with a skill, stat or the like new or changed.
fn changed_characters(before: Option<&ServerRows>, after: &ServerRows) -> Vec<String> {
    let stamp = |rows: &ServerRows, id: &str| {
        let stats = rows
            .characters
            .stats
            .iter()
            .filter(|s| s.character_id == id);
        let skills = rows
            .characters
            .skills
            .iter()
            .filter(|s| s.character_id == id);
        stats
            .map(|stat| stat.updated)
            .chain(skills.map(|skill| skill.updated))
            .max()
    };
    after
        .characters
        .characters
        .iter()
        .filter(|character| {
            let now = stamp(after, &character.id);
            now.is_some() && now != before.and_then(|rows| stamp(rows, &character.id))
        })
        .map(|character| character.name.clone())
        .collect()
}

fn in_hours(played: Duration) -> String {
    let minutes = played.as_secs() / 60;
    format!("{}h{:02}", minutes / 60, minutes % 60)
}
