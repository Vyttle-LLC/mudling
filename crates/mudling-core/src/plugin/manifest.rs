//! A plugin on disk: a folder with a `plugin.toml` manifest and one TOML file
//! per extension point it extends.
//!
//! ```toml
//! id = "testland"
//! name = "Testland"
//! version = "1.0.0"
//! api = 1
//! license = "MIT"
//! layer = "game"          # "default", "game" or "override"
//! encoding = "latin-1"    # optional: "utf-8" or "latin-1"
//! # Optional: what Luau hooks may do (see `plugin::luau`).
//! permissions = ["read_state", "write_store:skills", "send_commands"]
//!
//! [servers]               # which servers it's for; not for defaults
//! hosts = ["mud.testland.example", "*.testland.example"]
//! mssp_names = ["Testland"]
//!
//! [points]                # each extension point and the file declaring it
//! text = "text.toml"
//!
//! [hooks]                 # optional: the Luau files defining hooks
//! files = ["skills.luau"]
//! ```
//!
//! Loading validates everything, every pattern included, and reports the
//! first problem as a [`PluginError`] naming the file. It never panics.

use super::luau::{self, Script};
use super::points::{
    Character, Commands, Connection, Gear, Hud, Map, Mobs, Point, Points, Text, Ui,
};
use crate::line::Encoding;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use std::fmt;
use std::io;
use std::path::{Component, Path};
use std::sync::Arc;

/// The plugin API this core implements.
pub const API_VERSION: i64 = 1;
pub const MANIFEST_FILE: &str = "plugin.toml";

/// Who a plugin speaks for. Higher layers win where only one value applies,
/// and run later where many do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Layer {
    /// Built in and on for every server.
    Default,
    /// One game's plugin, on for its servers.
    Game,
    /// The player's own changes for one server.
    Override,
}

/// A plugin's id: lowercase letters, digits, `.`, `_` and `-`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Deserialize)]
#[serde(try_from = "String")]
pub struct PluginId(Arc<str>);

impl PluginId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for PluginId {
    type Error = String;

    fn try_from(id: String) -> Result<PluginId, String> {
        let mut chars = id.chars();
        let valid = chars
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            && chars.all(|c| {
                c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-')
            });
        if valid {
            Ok(PluginId(id.into()))
        } else {
            Err(format!(
                "`{id}` isn't a plugin id: use lowercase letters, digits, `.`, `_` and `-`"
            ))
        }
    }
}

impl fmt::Display for PluginId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
enum EncodingName {
    #[serde(rename = "utf-8")]
    Utf8,
    #[serde(rename = "latin-1")]
    Latin1,
}

impl From<EncodingName> for Encoding {
    fn from(name: EncodingName) -> Encoding {
        match name {
            EncodingName::Utf8 => Encoding::Utf8,
            EncodingName::Latin1 => Encoding::Latin1,
        }
    }
}

/// The servers a game plugin or override is for. A host matches exactly or,
/// written `*.example.org`, by suffix; MSSP names match exactly. Both ignore
/// case.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Servers {
    #[serde(default)]
    pub hosts: Vec<String>,
    #[serde(default)]
    pub mssp_names: Vec<String>,
}

/// What the core knows about a server before any plugin runs.
#[derive(Clone, Copy, Debug, Default)]
pub struct ServerInfo<'a> {
    pub host: &'a str,
    pub mssp_name: Option<&'a str>,
}

impl Servers {
    fn is_empty(&self) -> bool {
        self.hosts.is_empty() && self.mssp_names.is_empty()
    }

    /// A host is exact (`mud.example.org`) or has one leading wildcard label
    /// (`*.example.org`); no other `*` is allowed.
    fn validate(&self) -> Result<(), String> {
        for host in &self.hosts {
            let domain = host.strip_prefix("*.").unwrap_or(host);
            if domain.is_empty() || domain.contains('*') {
                return Err(format!(
                    "the host \"{host}\" isn't one; write an exact host (\"mud.example.org\") \
                     or a leading wildcard (\"*.example.org\")"
                ));
            }
        }
        Ok(())
    }

    fn matches(&self, server: ServerInfo<'_>) -> bool {
        let host_matches = |pattern: &String| match pattern.strip_prefix('*') {
            Some(suffix) => {
                server.host.len() > suffix.len() && ends_with_ignoring_case(server.host, suffix)
            }
            None => pattern.eq_ignore_ascii_case(server.host),
        };
        self.hosts.iter().any(host_matches)
            || server.mssp_name.is_some_and(|name| {
                self.mssp_names
                    .iter()
                    .any(|known| known.eq_ignore_ascii_case(name))
            })
    }
}

fn ends_with_ignoring_case(text: &str, suffix: &str) -> bool {
    text.len() >= suffix.len()
        && text.is_char_boundary(text.len() - suffix.len())
        && text[text.len() - suffix.len()..].eq_ignore_ascii_case(suffix)
}

/// The file declaring each extension point, relative to the plugin folder.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PointFiles {
    pub connection: Option<String>,
    pub commands: Option<String>,
    pub text: Option<String>,
    pub hud: Option<String>,
    pub map: Option<String>,
    pub character: Option<String>,
    pub gear: Option<String>,
    pub mobs: Option<String>,
    pub ui: Option<String>,
}

impl PointFiles {
    fn get(&self, point: Point) -> Option<&str> {
        let file = match point {
            Point::Connection => &self.connection,
            Point::Commands => &self.commands,
            Point::Text => &self.text,
            Point::Hud => &self.hud,
            Point::Map => &self.map,
            Point::Character => &self.character,
            Point::Gear => &self.gear,
            Point::Mobs => &self.mobs,
            Point::Ui => &self.ui,
        };
        file.as_deref()
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub id: PluginId,
    pub name: String,
    pub version: String,
    pub api: i64,
    pub license: String,
    pub layer: Layer,
    #[serde(default)]
    encoding: Option<EncodingName>,
    #[serde(default)]
    pub servers: Servers,
    #[serde(default)]
    pub points: PointFiles,
    #[serde(default)]
    pub permissions: Vec<Permission>,
    #[serde(default)]
    pub hooks: HookFiles,
}

/// The Luau files a plugin's hooks are in, loaded in order into one VM.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookFiles {
    #[serde(default)]
    pub files: Vec<String>,
}

/// What a plugin's Luau hooks may do beyond reading their event.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Deserialize)]
#[serde(try_from = "String")]
pub enum Permission {
    /// Read the state view the caller supplies.
    ReadState,
    /// Write to the named store.
    WriteStore(String),
    /// Send commands to the game; needs the player's approval at install.
    SendCommands,
}

impl TryFrom<String> for Permission {
    type Error = String;

    fn try_from(permission: String) -> Result<Permission, String> {
        match permission.as_str() {
            "read_state" => Ok(Permission::ReadState),
            "send_commands" => Ok(Permission::SendCommands),
            other => match other.strip_prefix("write_store:") {
                Some(store) if !store.trim().is_empty() => {
                    Ok(Permission::WriteStore(store.to_owned()))
                }
                _ => Err(format!(
                    "`{permission}` isn't a permission: use \"read_state\", \"write_store:<name>\" or \"send_commands\""
                )),
            },
        }
    }
}

impl Manifest {
    /// The text encoding the plugin sets, if any. Only one applies: the
    /// highest layer's.
    pub fn encoding(&self) -> Option<Encoding> {
        self.encoding.map(Encoding::from)
    }

    /// Whether the plugin is for `server`. Default plugins are for every one.
    pub fn matches_server(&self, server: ServerInfo<'_>) -> bool {
        self.layer == Layer::Default || self.servers.matches(server)
    }

    fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("`name` is empty".into());
        }
        if self.license.trim().is_empty() {
            return Err("`license` is empty".into());
        }
        let semver = self.version.split('.').collect::<Vec<_>>();
        if semver.len() != 3
            || semver
                .iter()
                .any(|part| part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()))
        {
            return Err(format!(
                "`version = \"{}\"` isn't MAJOR.MINOR.PATCH, such as \"1.0.0\"",
                self.version
            ));
        }
        match (self.layer, self.servers.is_empty()) {
            (Layer::Default, false) => {
                return Err(
                    "a default plugin is for every server, so it can't list `[servers]`".into(),
                );
            }
            (Layer::Game | Layer::Override, true) => {
                return Err(
                    "a game plugin or override must list its `[servers]` (hosts or mssp_names)"
                        .into(),
                );
            }
            _ => {}
        }
        self.servers.validate()?;
        for point in Point::ALL {
            if let Some(file) = self.points.get(point) {
                check_relative(file, "toml")
                    .map_err(|problem| format!("points.{point} = \"{file}\": {problem}"))?;
            }
        }
        for file in &self.hooks.files {
            check_relative(file, "luau")
                .map_err(|problem| format!("hooks.files: \"{file}\": {problem}"))?;
        }
        Ok(())
    }
}

/// A point or hook file must be inside the plugin folder, with `extension`.
fn check_relative(file: &str, extension: &str) -> Result<(), String> {
    let path = Path::new(file);
    if !path
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err("the file must be inside the plugin folder".into());
    }
    if path.extension().is_none_or(|found| found != extension) {
        return Err(format!("the file must be a .{extension} file"));
    }
    Ok(())
}

/// A loaded, validated plugin.
#[derive(Clone, Debug)]
pub struct Plugin {
    pub manifest: Manifest,
    pub points: Points,
    /// The `[hooks]` files, in order, each checked to compile.
    pub scripts: Vec<Script>,
}

impl Plugin {
    /// Loads the plugin in `dir`.
    pub fn load(dir: impl AsRef<Path>) -> Result<Plugin, PluginError> {
        let dir = dir.as_ref();
        Plugin::from_files(|file| std::fs::read_to_string(dir.join(file))).map_err(|mut error| {
            error.source = dir.join(&error.source).display().to_string();
            error
        })
    }

    /// Loads a plugin whose files come from `read`, given each file's name
    /// relative to the plugin folder (`plugin.toml` first). Built-in plugins
    /// load this way from text compiled into the app.
    pub fn from_files(
        mut read: impl FnMut(&str) -> io::Result<String>,
    ) -> Result<Plugin, PluginError> {
        let manifest_text = read_file(&mut read, MANIFEST_FILE)?;
        check_api(&manifest_text)?;
        let manifest: Manifest = parse(MANIFEST_FILE, &manifest_text)?;
        manifest
            .validate()
            .map_err(|problem| PluginError::invalid(MANIFEST_FILE, problem))?;

        let files = &manifest.points;
        let mut load = |point: Point| -> Result<Option<(String, String)>, PluginError> {
            files
                .get(point)
                .map(|file| Ok((file.to_owned(), read_file(&mut read, file)?)))
                .transpose()
        };
        let points = Points {
            connection: point::<Connection>(load(Point::Connection)?)?,
            commands: point::<Commands>(load(Point::Commands)?)?,
            text: point_checked(load(Point::Text)?, Text::validate)?,
            hud: point::<Hud>(load(Point::Hud)?)?,
            map: point::<Map>(load(Point::Map)?)?,
            character: point_checked(load(Point::Character)?, Character::validate)?,
            gear: point::<Gear>(load(Point::Gear)?)?,
            mobs: point::<Mobs>(load(Point::Mobs)?)?,
            ui: point::<Ui>(load(Point::Ui)?)?,
        };
        let mut scripts = Vec::with_capacity(manifest.hooks.files.len());
        for file in &manifest.hooks.files {
            let source = read_file(&mut read, file)?;
            luau::check_syntax(file, &source)
                .map_err(|problem| PluginError::invalid(file, problem))?;
            scripts.push(Script {
                file: file.clone(),
                source: source.into(),
            });
        }
        Ok(Plugin {
            manifest,
            points,
            scripts,
        })
    }

    pub fn id(&self) -> &PluginId {
        &self.manifest.id
    }

    pub fn layer(&self) -> Layer {
        self.manifest.layer
    }
}

fn read_file(
    read: &mut impl FnMut(&str) -> io::Result<String>,
    file: &str,
) -> Result<String, PluginError> {
    read(file).map_err(|error| PluginError {
        source: file.to_owned(),
        problem: Problem::Read(error.to_string()),
    })
}

/// Reads `api` before anything else, so a plugin for another API version
/// gets that error rather than one about a field this version doesn't know.
fn check_api(manifest_text: &str) -> Result<(), PluginError> {
    let table: toml::Table = parse(MANIFEST_FILE, manifest_text)?;
    match table.get("api") {
        None => Err(PluginError::invalid(
            MANIFEST_FILE,
            format!("`api` is missing; write `api = {API_VERSION}`"),
        )),
        Some(toml::Value::Integer(API_VERSION)) => Ok(()),
        Some(other) => {
            let found = match other {
                toml::Value::Integer(version) => version.to_string(),
                other => format!("a {}", other.type_str()),
            };
            Err(PluginError {
                source: MANIFEST_FILE.into(),
                problem: Problem::UnsupportedApi(found),
            })
        }
    }
}

fn parse<T: DeserializeOwned>(file: &str, text: &str) -> Result<T, PluginError> {
    toml::from_str(text).map_err(|error| PluginError {
        source: file.to_owned(),
        problem: Problem::Toml(error.to_string().trim_end().to_owned()),
    })
}

fn point<T: DeserializeOwned>(source: Option<(String, String)>) -> Result<Option<T>, PluginError> {
    source.map(|(file, text)| parse(&file, &text)).transpose()
}

fn point_checked<T: DeserializeOwned>(
    source: Option<(String, String)>,
    validate: fn(&T) -> Result<(), String>,
) -> Result<Option<T>, PluginError> {
    let Some((file, text)) = source else {
        return Ok(None);
    };
    let declaration = parse(&file, &text)?;
    validate(&declaration).map_err(|problem| PluginError::invalid(&file, problem))?;
    Ok(Some(declaration))
}

/// Why a plugin was rejected, and where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginError {
    /// Where the problem is: the file (with the plugin folder's path when
    /// loaded from one), the plugin's id for a [`Problem::DuplicateId`], or
    /// `plugin host` for a [`Problem::Matcher`].
    pub source: String,
    pub problem: Problem,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Problem {
    /// The file couldn't be read.
    Read(String),
    /// The TOML is malformed, or doesn't fit the schema; the message gives
    /// the line and column.
    Toml(String),
    /// `api` names a version this core doesn't implement.
    UnsupportedApi(String),
    /// The file parsed, but a value isn't allowed.
    Invalid(String),
    /// Another plugin in the host already has this id.
    DuplicateId(PluginId),
    /// The merged matcher couldn't be built, such as a set of patterns too
    /// large to compile together.
    Matcher(String),
}

impl PluginError {
    fn invalid(file: &str, problem: impl Into<String>) -> PluginError {
        PluginError {
            source: file.to_owned(),
            problem: Problem::Invalid(problem.into()),
        }
    }
}

impl fmt::Display for PluginError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.problem {
            Problem::Read(error) => write!(f, "{}: couldn't read the file: {error}", self.source),
            Problem::Toml(message) => write!(f, "{}: {message}", self.source),
            Problem::UnsupportedApi(found) => write!(
                f,
                "{}: `api = {found}` isn't supported; this MUDling reads plugins written for api = {API_VERSION}",
                self.source
            ),
            Problem::Invalid(message) => write!(f, "{}: {message}", self.source),
            Problem::DuplicateId(id) => write!(
                f,
                "{}: another plugin already has the id `{id}`",
                self.source
            ),
            Problem::Matcher(message) => write!(
                f,
                "{}: the patterns couldn't be compiled together: {message}",
                self.source
            ),
        }
    }
}

impl std::error::Error for PluginError {}
