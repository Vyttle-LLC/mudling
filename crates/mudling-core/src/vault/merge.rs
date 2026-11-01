//! Merging two copies of `secrets.vault`, which takes no key: sealed entries
//! are moved as they are, never opened.
//!
//! While both sides share a vault key, entries merge one by one against the
//! copy they both started from. Once a master password change has replaced the
//! key on one side, the new key's file wins, and what the other side saved
//! since they parted goes into `stranded`, because only its own key can open
//! it. [`Vault::stranded`] counts those, so they're never silently lost.
//!
//! [`Vault::stranded`]: super::Vault::stranded

use super::error::Error;
use super::file::{self, Sealed, VaultFile};
use crate::merge::three_way;
use std::collections::{BTreeMap, BTreeSet};

/// Merges the bytes of two vaults that started from `base`. An unreadable
/// `base` counts as none, which at worst keeps a deleted entry.
pub(crate) fn merge_files(
    base: Option<&[u8]>,
    ours: &[u8],
    theirs: &[u8],
    theirs_newer: bool,
) -> Result<Vec<u8>, Error> {
    let base = base.and_then(|base| file::parse(base).ok());
    let merged = merge(
        base.as_ref(),
        file::parse(ours)?,
        file::parse(theirs)?,
        theirs_newer,
    );
    Ok(file::to_bytes(&merged)?)
}

fn merge(
    base: Option<&VaultFile>,
    ours: VaultFile,
    theirs: VaultFile,
    theirs_newer: bool,
) -> VaultFile {
    let stranded = merge_stranded(base, &ours, &theirs, theirs_newer);
    if ours.same_key(&theirs) {
        let mut merged = ours;
        merged.entries = merge_entries(
            shared_entries(base, &merged),
            &merged.entries,
            &theirs.entries,
            theirs_newer,
        );
        merged.stranded = stranded;
        return merged;
    }
    let is_unchanged = |file: &VaultFile| base.is_some_and(|base| base.same_key(file));
    let ours_win = if is_unchanged(&ours) {
        false
    } else if is_unchanged(&theirs) {
        true
    } else {
        !theirs_newer
    };
    let (mut merged, loser) = if ours_win {
        (ours, theirs)
    } else {
        (theirs, ours)
    };
    merged.stranded = stranded;
    strand_changes(base, loser, &mut merged);
    merged
}

/// Moves what `loser` changed since `base` into `merged`'s `stranded`.
fn strand_changes(base: Option<&VaultFile>, mut loser: VaultFile, merged: &mut VaultFile) {
    let started_from = shared_entries(base, &loser);
    loser
        .entries
        .retain(|id, sealed| started_from.get(id) != Some(&*sealed));
    if loser.entries.is_empty() {
        return;
    }
    loser.stranded.clear();
    match merged.stranded.get_mut(&loser.stranded_id()) {
        Some(block) => block.entries.append(&mut loser.entries),
        None => {
            merged.stranded.insert(loser.stranded_id(), loser);
        }
    }
}

/// `base`'s entries if `file` is sealed under the same key, or none, because
/// entries under different keys have nothing in common.
fn shared_entries<'a>(
    base: Option<&'a VaultFile>,
    file: &VaultFile,
) -> &'a BTreeMap<String, Sealed> {
    static NONE: BTreeMap<String, Sealed> = BTreeMap::new();
    base.filter(|base| base.same_key(file))
        .map_or(&NONE, |base| &base.entries)
}

fn merge_entries(
    base: &BTreeMap<String, Sealed>,
    ours: &BTreeMap<String, Sealed>,
    theirs: &BTreeMap<String, Sealed>,
    theirs_newer: bool,
) -> BTreeMap<String, Sealed> {
    let ids: BTreeSet<&String> = ours.keys().chain(theirs.keys()).collect();
    ids.into_iter()
        .filter_map(|id| {
            let chosen = three_way(base.get(id), ours.get(id), theirs.get(id), theirs_newer)?;
            Some((id.clone(), chosen.clone()))
        })
        .collect()
}

/// Entries each side already had stranded. A side that re-sealed one has
/// dropped it, and that holds here like any other delete.
fn merge_stranded(
    base: Option<&VaultFile>,
    ours: &VaultFile,
    theirs: &VaultFile,
    theirs_newer: bool,
) -> BTreeMap<String, VaultFile> {
    let ids: BTreeSet<&String> = ours.stranded.keys().chain(theirs.stranded.keys()).collect();
    let mut merged = BTreeMap::new();
    for id in ids {
        let (our_block, their_block) = (ours.stranded.get(id), theirs.stranded.get(id));
        let block = our_block.or(their_block).expect("one side holds the id");
        let side = |block: Option<&VaultFile>| {
            block.map(|block| block.entries.clone()).unwrap_or_default()
        };
        let started_from = base.and_then(|base| base.stranded.get(id));
        let entries = merge_entries(
            &side(started_from),
            &side(our_block),
            &side(their_block),
            theirs_newer,
        );
        if !entries.is_empty() {
            let mut block = block.clone();
            block.entries = entries;
            merged.insert(id.clone(), block);
        }
    }
    merged
}
