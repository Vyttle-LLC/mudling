//! Merging two commits' files into the files of one.

use super::records::merge_server;
use super::rooms::retired_rooms;
use super::{Error, Retired};
use crate::git::Files;
use crate::repo::format::{
    MANIFEST, SERVERS, SETTINGS, ServerList, ServerRows, check_manifest, is_record_file, json,
    parse,
};
use crate::repo::{self, AppSettings};
use crate::vault;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// One side's choice between versions of a thing with no time of its own,
/// `None` meaning it doesn't exist. A side that changed it beats one that
/// didn't; a change beats a delete; if both changed it, the newer side wins.
pub(crate) fn three_way<'a, T: PartialEq>(
    base: Option<&'a T>,
    ours: Option<&'a T>,
    theirs: Option<&'a T>,
    theirs_newer: bool,
) -> Option<&'a T> {
    if ours == theirs || theirs == base {
        ours
    } else if ours == base {
        theirs
    } else {
        match (ours, theirs) {
            (Some(ours), Some(_)) if !theirs_newer => Some(ours),
            (_, Some(theirs)) => Some(theirs),
            (ours, None) => ours,
        }
    }
}

/// Merges `ours` and `theirs`, which both build on `base`. `base` is empty if
/// they share no history. Fails, having written nothing, on a file it can't
/// read, because guessing could lose records.
pub(super) fn merge_files(
    base: &Files,
    ours: &Files,
    theirs: &Files,
    theirs_newer: bool,
) -> Result<Files, Error> {
    let mut merged = file_by_file(base, ours, theirs, theirs_newer);
    check_format(ours)?;

    let our_servers = servers(ours)?;
    let their_servers = servers(theirs)?;
    for (_, folder) in our_servers.values().chain(their_servers.values()) {
        merged.retain(|path, _| !path.strip_prefix(folder).is_ok_and(is_record_file));
    }
    // A base that can't be read only means notes are gathered, not ranked.
    let base_servers = servers(base).unwrap_or_default();
    let list = merge_servers(base_servers, our_servers, their_servers, &mut merged);
    if ours.contains_key(Path::new(SERVERS)) || theirs.contains_key(Path::new(SERVERS)) {
        merged.insert(SERVERS.into(), json(&list));
    }

    if let Some(settings) = newer_settings(ours, theirs)? {
        merged.insert(SETTINGS.into(), settings);
    }
    let vault_path = Path::new(vault::FILE_NAME);
    if let (Some(ours), Some(theirs)) = (ours.get(vault_path), theirs.get(vault_path))
        && ours != theirs
    {
        let base = base.get(vault_path).map(Vec::as_slice);
        let bytes = vault::merge_files(base, ours, theirs, theirs_newer)?;
        merged.insert(vault_path.to_owned(), bytes);
    }
    Ok(merged)
}

/// Every path in any of the three, each settled by [`three_way`].
fn file_by_file(base: &Files, ours: &Files, theirs: &Files, theirs_newer: bool) -> Files {
    let paths: BTreeSet<&PathBuf> = base
        .keys()
        .chain(ours.keys())
        .chain(theirs.keys())
        .collect();
    paths
        .into_iter()
        .filter_map(|path| {
            three_way(
                base.get(path),
                ours.get(path),
                theirs.get(path),
                theirs_newer,
            )
            .map(|bytes| (path.clone(), bytes.clone()))
        })
        .collect()
}

/// Refuses what a newer MUDling wrote, which this one can't merge.
pub(super) fn check_format(files: &Files) -> Result<(), repo::Error> {
    check_manifest(files.get(Path::new(MANIFEST)).map(Vec::as_slice))
}

/// The rooms `before` holds that `after` doesn't, by server. A server `after`
/// doesn't list stays on this device, so its rooms stay too.
pub(super) fn retired(before: &Files, after: &Files) -> Result<Retired, repo::Error> {
    let after = servers(after)?;
    let mut retired = Retired::new();
    for (id, (rows, _)) in servers(before)? {
        if let Some((merged, _)) = after.get(&id) {
            let moves = retired_rooms(&rows, merged);
            if !moves.is_empty() {
                retired.insert(id, moves);
            }
        }
    }
    Ok(retired)
}

/// One side's servers by id, each with its folder.
pub(crate) fn servers(
    files: &Files,
) -> Result<BTreeMap<String, (ServerRows, PathBuf)>, repo::Error> {
    let mut list: ServerList = match files.get(Path::new(SERVERS)) {
        Some(bytes) => parse(Path::new(SERVERS), bytes)?,
        None => ServerList::default(),
    };
    list.sort();
    let mut servers = BTreeMap::new();
    for (entry, folder) in list.with_folders() {
        let held = files.iter().filter_map(|(path, bytes)| {
            let relative = path.strip_prefix(&folder).ok()?;
            Some((relative, bytes.as_slice()))
        });
        let rows = ServerRows::parse(entry, held)?;
        servers.insert(entry.server.id.clone(), (rows, folder));
    }
    Ok(servers)
}

/// Adds every server's merged files to `merged`, returning the merged list.
/// A server only one side has merges with nothing, which keeps its records
/// and drops any row a hand-edited file left with no parent.
fn merge_servers(
    base: BTreeMap<String, (ServerRows, PathBuf)>,
    mut ours: BTreeMap<String, (ServerRows, PathBuf)>,
    mut theirs: BTreeMap<String, (ServerRows, PathBuf)>,
    merged: &mut Files,
) -> ServerList {
    let ids: BTreeSet<String> = ours.keys().chain(theirs.keys()).cloned().collect();
    let servers: Vec<ServerRows> = ids
        .iter()
        .map(|id| {
            let rows = |side: &mut BTreeMap<_, (ServerRows, PathBuf)>| {
                side.remove(id).map(|(rows, _)| rows).unwrap_or_default()
            };
            let base_notes = base.get(id).map_or(&[][..], |(rows, _)| &rows.map.notes);
            merge_server(rows(&mut ours), rows(&mut theirs), base_notes)
        })
        .collect();
    let list = ServerList {
        servers: servers
            .iter()
            .flat_map(|rows| rows.servers.iter().map(|server| rows.entry(server)))
            .collect(),
    };
    for ((_, folder), rows) in list.with_folders().zip(&servers) {
        for (path, bytes) in rows.files() {
            merged.insert(folder.join(path), bytes);
        }
    }
    list
}

/// Settings are one record, so the newer `updated` takes all of them. A tie
/// settles on the same side on every device.
fn newer_settings(ours: &Files, theirs: &Files) -> Result<Option<Vec<u8>>, repo::Error> {
    let path = Path::new(SETTINGS);
    let read = |files: &Files| -> Result<_, repo::Error> {
        let Some(bytes) = files.get(path) else {
            return Ok(None);
        };
        let settings: AppSettings = parse(path, bytes)?;
        Ok(Some((settings.updated, bytes.clone())))
    };
    Ok(read(ours)?.max(read(theirs)?).map(|(_, bytes)| bytes))
}

#[cfg(test)]
mod tests {
    use super::three_way;

    fn pick(
        base: Option<u8>,
        ours: Option<u8>,
        theirs: Option<u8>,
        theirs_newer: bool,
    ) -> Option<u8> {
        three_way(base.as_ref(), ours.as_ref(), theirs.as_ref(), theirs_newer).copied()
    }

    #[test]
    fn the_side_that_changed_something_wins() {
        assert_eq!(pick(Some(1), Some(1), Some(2), false), Some(2));
        assert_eq!(pick(Some(1), Some(2), Some(1), true), Some(2));
        assert_eq!(pick(None, None, Some(2), false), Some(2));
    }

    #[test]
    fn a_delete_holds_against_no_change_but_not_against_a_change() {
        assert_eq!(pick(Some(1), None, Some(1), true), None);
        assert_eq!(pick(Some(1), Some(1), None, true), None);
        assert_eq!(pick(Some(1), None, Some(2), false), Some(2));
        assert_eq!(pick(Some(1), Some(2), None, true), Some(2));
    }

    #[test]
    fn when_both_changed_it_the_newer_side_wins() {
        assert_eq!(pick(Some(1), Some(2), Some(3), true), Some(3));
        assert_eq!(pick(Some(1), Some(2), Some(3), false), Some(2));
        assert_eq!(pick(None, Some(2), Some(3), true), Some(3));
    }
}
