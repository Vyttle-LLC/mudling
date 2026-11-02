//! The registry against a local repo laid out as `mudling-plugins` is, signed
//! with throwaway keys.

use super::*;
use crate::git::{Limit, Oid};
use ed25519_dalek::{Signer, SigningKey};
use git2::Repository;
use serde_json::json;
use std::collections::BTreeMap;
use std::time::Duration;
use tempfile::TempDir;

const MANIFEST: &str = "id = \"tells\"\nname = \"Tell highlighter\"\nversion = \"1.0.0\"\napi = 1\n\
    license = \"MIT\"\nlayer = \"default\"\n[points]\ntext = \"text.toml\"\n";
const PATTERNS: &str = "[[pattern]]\nname = \"tell\"\npattern = '''^\\w+ tells you'''\n";
/// What a tarball or a checkout would rewrite: `git archive` fills in
/// `$Format$` and drops `export-ignore`d files, and a checkout turns line
/// endings to CRLF.
const ATTRIBUTES: &str = "* text eol=crlf\nnotes.txt export-subst\nhidden.txt export-ignore\n";

fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn limit() -> Limit {
    Limit::after(Duration::from_secs(30))
}

/// A registry repo with `tells` on `main`, and its signed index on `index`.
struct Upstream {
    folder: TempDir,
    repo: Repository,
    main: Oid,
}

impl Upstream {
    fn new() -> Upstream {
        let folder = TempDir::new().unwrap();
        let repo = Repository::init_bare(folder.path()).unwrap();
        let main = commit(&repo, "refs/heads/main", &plugin_files());
        Upstream { folder, repo, main }
    }

    fn url(&self) -> String {
        self.folder.path().display().to_string()
    }

    /// The index for `main`, with `tells`' entry as `change` leaves it.
    fn signed(&self, published_at: &str, change: impl FnOnce(&mut serde_json::Value)) -> String {
        let mut entry = json!({
            "id": "tells",
            "name": "Tell highlighter",
            "version": "1.0.0",
            "license": "MIT",
            "games": {"hosts": [], "mssp_names": []},
            "source": "plugins/tells",
            "hash": content_hash(&folder_of(&plugin_files(), "plugins/tells/")),
        });
        change(&mut entry);
        serde_json::to_string_pretty(&json!({
            "format": 1,
            "commit": self.main.to_string(),
            "published_at": published_at,
            "plugins": [entry],
        }))
        .unwrap()
    }

    fn publish(&self, signed: &str, by: &SigningKey) {
        let signature = data_encoding::BASE64.encode(&by.sign(signed.as_bytes()).to_bytes());
        let file = json!({"signed": signed, "signature": signature});
        let bytes = serde_json::to_vec_pretty(&file).unwrap();
        commit(&self.repo, "refs/heads/index", &[("index.json", bytes)]);
    }
}

fn plugin_files() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("README.md", b"# The registry\n".to_vec()),
        ("plugins/tells/plugin.toml", MANIFEST.into()),
        ("plugins/tells/text.toml", PATTERNS.into()),
        ("plugins/tells/.gitattributes", ATTRIBUTES.into()),
        (
            "plugins/tells/notes.txt",
            b"built from $Format:%H$\n".to_vec(),
        ),
        ("plugins/tells/hidden.txt", b"kept\n".to_vec()),
    ]
}

fn folder_of(files: &[(&str, Vec<u8>)], prefix: &str) -> FolderFiles {
    files
        .iter()
        .filter_map(|(path, bytes)| Some((path.strip_prefix(prefix)?.to_owned(), bytes.clone())))
        .collect()
}

/// Commits `files` to `reference` as the whole tree, with git's own bytes.
fn commit(repo: &Repository, reference: &str, files: &[(&str, Vec<u8>)]) -> Oid {
    let mut root = Folder::default();
    for (path, bytes) in files {
        root.add(path, repo.blob(bytes).unwrap());
    }
    let tree = repo.find_tree(root.write(repo)).unwrap();
    let signature = git2::Signature::now("Test", "test@example.org").unwrap();
    let parent = repo
        .refname_to_id(reference)
        .ok()
        .map(|id| repo.find_commit(id).unwrap());
    let parents: Vec<&git2::Commit> = parent.iter().collect();
    repo.commit(
        Some(reference),
        &signature,
        &signature,
        "Publish",
        &tree,
        &parents,
    )
    .unwrap()
}

#[derive(Default)]
struct Folder {
    files: BTreeMap<String, Oid>,
    folders: BTreeMap<String, Folder>,
}

impl Folder {
    fn add(&mut self, path: &str, blob: Oid) {
        match path.split_once('/') {
            Some((folder, rest)) => self
                .folders
                .entry(folder.to_owned())
                .or_default()
                .add(rest, blob),
            None => {
                self.files.insert(path.to_owned(), blob);
            }
        }
    }

    fn write(&self, repo: &Repository) -> Oid {
        let mut builder = repo.treebuilder(None).unwrap();
        for (name, blob) in &self.files {
            builder.insert(name, *blob, 0o100_644).unwrap();
        }
        for (name, folder) in &self.folders {
            builder.insert(name, folder.write(repo), 0o040_000).unwrap();
        }
        builder.write().unwrap()
    }
}

fn registry(upstream: &Upstream, data: &TempDir, key: Option<&SigningKey>) -> Registry {
    Registry::at(
        data.path(),
        &upstream.url(),
        key.map(SigningKey::verifying_key),
    )
}

#[test]
fn an_index_the_registry_signed_lists_its_plugins() {
    let upstream = Upstream::new();
    upstream.publish(&upstream.signed("2026-11-02T17:04:31Z", |_| {}), &key(1));
    let data = TempDir::new().unwrap();

    let index = registry(&upstream, &data, Some(&key(1)))
        .refresh(&limit())
        .unwrap();

    assert_eq!(index.commit, upstream.main);
    assert_eq!(index.published_at, "2026-11-02T17:04:31Z");
    let names: Vec<&str> = index
        .plugins
        .iter()
        .map(|entry| entry.name.as_str())
        .collect();
    assert_eq!(names, ["Tell highlighter"]);
}

#[test]
fn an_index_signed_with_another_key_is_refused() {
    let upstream = Upstream::new();
    upstream.publish(&upstream.signed("2026-11-02T17:04:31Z", |_| {}), &key(2));
    let data = TempDir::new().unwrap();

    let refused = registry(&upstream, &data, Some(&key(1))).refresh(&limit());

    assert_eq!(refused, Err(IndexError::Forged));
}

#[test]
fn an_index_changed_after_it_was_signed_is_refused() {
    let upstream = Upstream::new();
    let signed = upstream.signed("2026-11-02T17:04:31Z", |_| {});
    let signature = data_encoding::BASE64.encode(&key(1).sign(signed.as_bytes()).to_bytes());
    let changed = signed.replace("Tell highlighter", "Tell highlighted");
    let file = serde_json::to_vec(&json!({"signed": changed, "signature": signature})).unwrap();

    let refused = verify(&file, &key(1).verifying_key(), None);

    assert_eq!(refused, Err(IndexError::Forged));
}

#[test]
fn an_index_older_than_one_seen_here_is_refused() {
    let upstream = Upstream::new();
    let data = TempDir::new().unwrap();
    let registry = registry(&upstream, &data, Some(&key(1)));
    upstream.publish(&upstream.signed("2026-11-02T17:04:31Z", |_| {}), &key(1));
    registry.refresh(&limit()).unwrap();

    // A replay of an older index, signed in its day.
    upstream.publish(&upstream.signed("2026-10-30T09:00:00Z", |_| {}), &key(1));
    let refused = registry.refresh(&limit());

    assert_eq!(
        refused,
        Err(IndexError::Stale {
            seen: "2026-11-02T17:04:31Z".to_owned(),
            offered: "2026-10-30T09:00:00Z".to_owned(),
        })
    );
    // And a refused one doesn't become the newest seen.
    upstream.publish(&upstream.signed("2026-11-01T00:00:00Z", |_| {}), &key(1));
    assert!(matches!(
        registry.refresh(&limit()),
        Err(IndexError::Stale { .. })
    ));
}

#[test]
fn a_forged_index_never_becomes_the_newest_seen() {
    let upstream = Upstream::new();
    let data = TempDir::new().unwrap();
    let registry = registry(&upstream, &data, Some(&key(1)));
    upstream.publish(&upstream.signed("2030-01-01T00:00:00Z", |_| {}), &key(2));
    assert_eq!(registry.refresh(&limit()), Err(IndexError::Forged));

    upstream.publish(&upstream.signed("2026-11-02T17:04:31Z", |_| {}), &key(1));

    assert!(registry.refresh(&limit()).is_ok());
}

#[test]
fn an_index_in_a_format_this_app_does_not_know_is_refused() {
    let signed = r#"{"format": 2, "commit": "x", "published_at": "x", "plugins": []}"#;
    let signature = data_encoding::BASE64.encode(&key(1).sign(signed.as_bytes()).to_bytes());
    let file = serde_json::to_vec(&json!({"signed": signed, "signature": signature})).unwrap();

    let refused = verify(&file, &key(1).verifying_key(), None);

    assert_eq!(refused, Err(IndexError::UnknownFormat(2)));
}

#[test]
fn without_a_key_every_index_is_refused_before_the_network() {
    let data = TempDir::new().unwrap();
    // A registry that can't be reached: no fetch is tried.
    let registry = Registry::at(data.path(), "/nowhere/at/all", None);

    assert_eq!(registry.refresh(&limit()), Err(IndexError::NoKey));
    assert_eq!(REGISTRY_PUBLIC_KEY, None, "set it, and drop this line");
}

#[test]
fn the_content_hash_is_the_registry_recipe() {
    let files: FolderFiles = [
        ("d/b.lua".to_owned(), b"x".to_vec()),
        ("a.txt".to_owned(), b"hi\n".to_vec()),
    ]
    .into();

    // `find . -type f | sed 's|^\./||' | LC_ALL=C sort | xargs shasum -a 256 | shasum -a 256`
    assert_eq!(
        content_hash(&files),
        "sha256:c6cebbe5cfcf51694cfafae0a6e933e38bde84e1221c69fb1cf427bb7af5f406"
    );
}

#[test]
fn a_plugin_installs_with_the_bytes_git_holds_whatever_its_attributes_say() {
    let upstream = Upstream::new();
    upstream.publish(&upstream.signed("2026-11-02T17:04:31Z", |_| {}), &key(1));
    let data = TempDir::new().unwrap();
    let registry = registry(&upstream, &data, Some(&key(1)));
    let index = registry.refresh(&limit()).unwrap();

    let fetched = registry.fetch(&index, &index.plugins[0], &limit()).unwrap();
    let installed = InstalledPlugins::in_data_folder(data.path());
    installed.install(&fetched).unwrap();

    let folder = installed.folder().join("tells");
    assert_eq!(
        fs::read_to_string(folder.join("notes.txt")).unwrap(),
        "built from $Format:%H$\n"
    );
    assert!(folder.join("hidden.txt").exists());
    assert_eq!(
        fs::read_to_string(folder.join("text.toml")).unwrap(),
        PATTERNS
    );
    assert_eq!(Plugin::load(&folder).unwrap().id().as_str(), "tells");
    assert_eq!(
        installed.records().get("tells"),
        Some(&Record::Registry {
            version: "1.0.0".to_owned(),
            commit: upstream.main.to_string(),
            hash: index.plugins[0].hash.clone(),
        })
    );
}

#[test]
fn a_plugin_that_does_not_match_its_hash_is_not_installed() {
    let upstream = Upstream::new();
    let wrong = format!("sha256:{}", "0".repeat(64));
    upstream.publish(
        &upstream.signed("2026-11-02T17:04:31Z", |entry| entry["hash"] = json!(wrong)),
        &key(1),
    );
    let data = TempDir::new().unwrap();
    let registry = registry(&upstream, &data, Some(&key(1)));
    let index = registry.refresh(&limit()).unwrap();

    let refused = registry.fetch(&index, &index.plugins[0], &limit());

    assert!(
        matches!(refused, Err(InstallError::HashMismatch { expected, .. }) if expected == wrong)
    );
    let installed = InstalledPlugins::in_data_folder(data.path());
    assert!(installed.plugin_folders().is_empty());
}

#[test]
fn installing_again_replaces_the_old_copy_whole() {
    let upstream = Upstream::new();
    upstream.publish(&upstream.signed("2026-11-02T17:04:31Z", |_| {}), &key(1));
    let data = TempDir::new().unwrap();
    let registry = registry(&upstream, &data, Some(&key(1)));
    let index = registry.refresh(&limit()).unwrap();
    let installed = InstalledPlugins::in_data_folder(data.path());
    installed
        .install(&registry.fetch(&index, &index.plugins[0], &limit()).unwrap())
        .unwrap();
    fs::write(installed.folder().join("tells/left-over.txt"), "old").unwrap();

    installed
        .install(&registry.fetch(&index, &index.plugins[0], &limit()).unwrap())
        .unwrap();

    assert!(!installed.folder().join("tells/left-over.txt").exists());
    assert_eq!(
        installed.plugin_folders(),
        [installed.folder().join("tells")]
    );
}

#[test]
fn an_approval_holds_for_the_copy_it_was_given_for() {
    let data = TempDir::new().unwrap();
    let approvals = Approvals::in_data_folder(data.path());

    approvals.approve("extras", "sha256:approved").unwrap();

    assert!(approvals.is_approved("extras", "sha256:approved"));
    assert!(
        !approvals.is_approved("extras", "sha256:changed"),
        "any change asks again"
    );
    approvals.take_back("extras").unwrap();
    assert!(!approvals.is_approved("extras", "sha256:approved"));
}

#[test]
fn the_folder_hash_of_an_installed_plugin_is_the_index_s() {
    let upstream = Upstream::new();
    upstream.publish(&upstream.signed("2026-11-02T17:04:31Z", |_| {}), &key(1));
    let data = TempDir::new().unwrap();
    let registry = registry(&upstream, &data, Some(&key(1)));
    let index = registry.refresh(&limit()).unwrap();
    let installed = InstalledPlugins::in_data_folder(data.path());

    installed
        .install(&registry.fetch(&index, &index.plugins[0], &limit()).unwrap())
        .unwrap();

    let folder = installed.folder().join("tells");
    assert_eq!(folder_hash(&folder).unwrap(), index.plugins[0].hash);
}

/// `tells` installed from a local registry, in a fresh data folder.
fn installed_tells() -> (TempDir, InstalledPlugins) {
    let upstream = Upstream::new();
    upstream.publish(&upstream.signed("2026-11-02T17:04:31Z", |_| {}), &key(1));
    let data = TempDir::new().unwrap();
    let registry = registry(&upstream, &data, Some(&key(1)));
    let index = registry.refresh(&limit()).unwrap();
    let installed = InstalledPlugins::in_data_folder(data.path());
    installed
        .install(&registry.fetch(&index, &index.plugins[0], &limit()).unwrap())
        .unwrap();
    (data, installed)
}

/// `tells` with no record, as a folder in a fresh data folder.
fn recordless_tells(data: &TempDir) -> InstalledPlugins {
    let installed = InstalledPlugins::in_data_folder(data.path());
    let folder = installed.folder().join("tells");
    fs::create_dir_all(&folder).unwrap();
    fs::write(folder.join("plugin.toml"), MANIFEST).unwrap();
    fs::write(folder.join("text.toml"), PATTERNS).unwrap();
    installed
}

#[test]
fn a_folder_with_no_record_made_on_this_device_is_the_player_s_own() {
    let data = TempDir::new().unwrap();
    let installed = recordless_tells(&data);

    assert_eq!(installed.load().plugins.len(), 1);
    fs::write(installed.folder().join("tells/notes.txt"), "edited").unwrap();
    assert_eq!(
        installed.load().plugins.len(),
        1,
        "editing it keeps it the player's"
    );
}

#[test]
fn a_folder_with_no_record_that_sync_wrote_is_not_loaded() {
    let data = TempDir::new().unwrap();
    Origins::in_data_folder(data.path())
        .mark_arrived(["tells"])
        .unwrap();
    let installed = recordless_tells(&data);

    let loaded = installed.load();

    assert!(loaded.plugins.is_empty());
    assert_eq!(loaded.arrived.len(), 1);
    installed.trust("tells", &loaded.arrived[0].hash).unwrap();
    assert_eq!(
        installed.load().plugins.len(),
        1,
        "the player trusted this copy"
    );
}

#[test]
fn a_trusted_copy_with_no_record_that_changes_afterwards_waits_again() {
    let data = TempDir::new().unwrap();
    Origins::in_data_folder(data.path())
        .mark_arrived(["tells"])
        .unwrap();
    let installed = recordless_tells(&data);
    let shown = installed.load().arrived[0].hash.clone();
    installed.trust("tells", &shown).unwrap();

    // A sync writes the folder right after the player's trust was checked.
    let swapped = "[[pattern]]\nname = \"tell\"\npattern = '''^.*$'''\n";
    fs::write(installed.folder().join("tells/text.toml"), swapped).unwrap();

    let loaded = installed.load();
    assert!(loaded.plugins.is_empty(), "only the copy shown was trusted");
    assert_eq!(loaded.arrived.len(), 1);
}

#[test]
fn trust_refuses_a_copy_that_changed_after_it_was_shown() {
    let data = TempDir::new().unwrap();
    Origins::in_data_folder(data.path())
        .mark_arrived(["tells"])
        .unwrap();
    let installed = recordless_tells(&data);
    let shown = installed.load().arrived[0].hash.clone();

    // A sync lands while the dialog is open.
    fs::write(installed.folder().join("tells/text.toml"), "[[pattern]]\n").unwrap();

    assert!(installed.trust("tells", &shown).is_err());
    assert!(installed.load().plugins.is_empty());
}

#[test]
fn trust_and_remove_act_on_the_folder_shown_not_the_one_named_for_the_id() {
    let data = TempDir::new().unwrap();
    Origins::in_data_folder(data.path())
        .mark_arrived(["aa", "zz"])
        .unwrap();
    let installed = InstalledPlugins::in_data_folder(data.path());
    // The repo brings `zz/`, holding plugin `aa`, and `aa/`, holding `bb`.
    let manifest = |id: &str| {
        format!(
            "id = \"{id}\"\nname = \"{id}\"\nversion = \"1.0.0\"\napi = 1\n\
             license = \"MIT\"\nlayer = \"default\"\n"
        )
    };
    for (folder, id) in [("zz", "aa"), ("aa", "bb")] {
        fs::create_dir_all(installed.folder().join(folder)).unwrap();
        fs::write(
            installed.folder().join(folder).join("plugin.toml"),
            manifest(id),
        )
        .unwrap();
    }
    let loaded = installed.load();
    let shown = (loaded.arrived.iter())
        .find(|copy| copy.plugin.id().as_str() == "aa")
        .unwrap();
    assert_eq!(shown.folder, "zz");

    installed.trust(&shown.folder, &shown.hash).unwrap();
    let ids: Vec<String> = (installed.load().plugins.iter())
        .map(|copy| copy.plugin.id().to_string())
        .collect();
    assert_eq!(
        ids,
        ["aa"],
        "plugin bb, which the player never saw, stays out"
    );

    installed.remove(&shown.folder).unwrap();
    assert!(!installed.folder().join("zz").exists());
    assert!(
        installed.folder().join("aa").exists(),
        "the other folder stays"
    );
}

#[test]
fn an_origins_file_that_cannot_be_read_holds_folders_back_and_stays_as_it_is() {
    let data = TempDir::new().unwrap();
    let unreadable = b"{\"arrived\": [\"tells\"], broken";
    fs::write(data.path().join("plugin-origins.json"), unreadable).unwrap();
    let installed = recordless_tells(&data);

    let loaded = installed.load();

    assert!(loaded.plugins.is_empty(), "not taken for the player's own");
    assert_eq!(
        fs::read(data.path().join("plugin-origins.json")).unwrap(),
        unreadable
    );
    assert!(
        Origins::in_data_folder(data.path())
            .mark_arrived(["tells"])
            .is_err()
    );
}

#[test]
fn a_copy_this_device_installed_loads() {
    let (_data, installed) = installed_tells();

    let loaded = installed.load();

    assert_eq!(loaded.plugins.len(), 1);
    assert!(loaded.arrived.is_empty() && loaded.refused.is_empty());
}

#[test]
fn a_tampered_copy_arriving_with_a_matching_record_is_not_loaded() {
    let (_data, installed) = installed_tells();

    // As a tampered repo would bring it: other patterns under the same name,
    // with a record rewritten to their hash.
    let swapped = "[[pattern]]\nname = \"tell\"\npattern = '''^.*$'''\n";
    fs::write(installed.folder().join("tells/text.toml"), swapped).unwrap();
    let hash = folder_hash(&installed.folder().join("tells")).unwrap();
    let record = Record::Registry {
        version: "1.0.0".to_owned(),
        commit: "0".repeat(40),
        hash: hash.clone(),
    };
    let record = serde_json::to_vec(&record).unwrap();
    fs::write(installed.folder().join("tells.json"), record).unwrap();

    let loaded = installed.load();

    assert!(loaded.plugins.is_empty());
    assert_eq!(loaded.arrived[0].plugin.id().as_str(), "tells");
    assert_eq!(loaded.arrived[0].hash, hash);

    installed.trust("tells", &hash).unwrap();
    assert_eq!(
        installed.load().plugins.len(),
        1,
        "the player trusted this copy"
    );
}

#[test]
fn a_copy_a_verified_index_lists_loads_wherever_it_came_from() {
    let upstream = Upstream::new();
    upstream.publish(&upstream.signed("2026-11-02T17:04:31Z", |_| {}), &key(1));
    let data = TempDir::new().unwrap();
    let installed = InstalledPlugins::in_data_folder(data.path());
    // Another device installed it, and sync brought the files.
    Origins::in_data_folder(data.path())
        .mark_arrived(["tells"])
        .unwrap();
    let mine = installed.folder().join("tells");
    for (path, bytes) in folder_of(&plugin_files(), "plugins/tells/") {
        fs::create_dir_all(mine.join(&path).parent().unwrap()).unwrap();
        fs::write(mine.join(path), bytes).unwrap();
    }
    assert_eq!(installed.load().arrived.len(), 1);

    registry(&upstream, &data, Some(&key(1)))
        .refresh(&limit())
        .unwrap();

    assert_eq!(installed.load().plugins.len(), 1);
}
