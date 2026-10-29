//! The default plugins: what makes a server useful with no game plugin, by
//! learning rather than asking (PRD §11, "Defaults that adapt").
//!
//! What the plugin API can express is a built-in plugin, [`plugin`], in the
//! default layer: text passes through uncolored, and the common pager
//! prompts are its pager, which a game plugin's replaces. What must be
//! learned at runtime is Rust, reading the same lines and settings:
//!
//! - [`prompt`]: the prompt, from the line after GMCP `Char.Vitals`;
//! - [`hud`]: which `Char.Vitals` fields hold which vitals;
//! - [`commands`]: chaining with the game's separator, and speedwalk;
//! - [`TablePager`]: answering the pager while a table is read;
//! - [`detect`]: which game plugin to suggest for a server.
//!
//! [`Defaults`] puts the per-line parts together. On the line thread, next
//! to the plugin host:
//!
//! ```text
//! for each finished line:   defaults.read_line(&line)
//! for each GMCP message:    defaults.gmcp(package, json), then check the pending text again
//! after each read, the pending text:
//!                           match defaults.read_pending(&pending, host.read_pending(&pending)) {
//!                               EndPrompt => pipeline.prompt_end(), then read that line too
//!                               Send(key) => send key as a line
//!                               Wait => {}
//!                           }
//! ```

pub mod commands;
pub mod detect;
pub mod hud;
pub mod prompt;

use crate::line::{Line, Pending};
use crate::plugin::{PendingKind, Plugin};
use hud::HudFields;
use prompt::PromptLearner;
use serde_json::{Map, Value};
use std::sync::Arc;

/// The built-in default plugin: the default Text layer.
pub fn plugin() -> Plugin {
    Plugin::from_files(|file| match file {
        "plugin.toml" => Ok(include_str!("plugin/plugin.toml").to_owned()),
        "text.toml" => Ok(include_str!("plugin/text.toml").to_owned()),
        _ => Err(std::io::Error::new(std::io::ErrorKind::NotFound, file.to_owned())),
    })
    .expect("the built-in defaults are valid")
}

/// What to do about the text the server hasn't ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PendingAction {
    Wait,
    /// It's a prompt: end it with `LinePipeline::prompt_end`.
    EndPrompt,
    /// It's the pager, during a table read: send this line to continue.
    Send(Arc<str>),
}

#[derive(Debug)]
pub struct Defaults {
    pub prompt: PromptLearner,
    pub hud: HudFields,
    pub pager: TablePager,
    /// Prompt lines are hidden by default: the HUD shows the same numbers
    /// (PRD §8). The shell reads this; a setting turns it off.
    pub hide_prompts: bool,
    gmcp_hello: Option<String>,
}

impl Default for Defaults {
    fn default() -> Defaults {
        Defaults {
            prompt: PromptLearner::default(),
            hud: HudFields::default(),
            pager: TablePager::default(),
            hide_prompts: true,
            gmcp_hello: None,
        }
    }
}

impl Defaults {
    /// A GMCP message from the server; `json` is its body.
    pub fn gmcp(&mut self, package: &str, json: &str) {
        let message: Option<Map<String, Value>> = serde_json::from_str(json).ok();
        if package.eq_ignore_ascii_case(hud::VITALS) {
            self.prompt.vitals_arrived();
            if let Some(message) = &message {
                self.hud.vitals_arrived(message);
            }
            return;
        }
        self.prompt.gmcp_arrived();
        if package.eq_ignore_ascii_case("Core.Hello")
            && let Some(Value::String(name)) = message.as_ref().and_then(|message| message.get("name"))
        {
            self.gmcp_hello = Some(name.clone());
        }
    }

    /// The game's name, if the server sent it in a GMCP `Core.Hello`.
    pub fn gmcp_hello(&self) -> Option<&str> {
        self.gmcp_hello.as_deref()
    }

    /// A finished line. Returns how many bytes of its plain text are a
    /// prompt at its start, for hiding it or reading what follows it.
    pub fn read_line(&mut self, line: &Line) -> Option<usize> {
        self.prompt.read_line(line)
    }

    /// The pending text, and what the plugin host made of it.
    pub fn read_pending(&mut self, pending: &Pending<'_>, kind: Option<PendingKind>) -> PendingAction {
        let learned = self.prompt.read_pending(pending);
        match kind {
            Some(PendingKind::Pager { key }) if self.pager.answer(pending.number) => PendingAction::Send(key),
            Some(PendingKind::Pager { .. }) => PendingAction::Wait,
            Some(PendingKind::Prompt) => PendingAction::EndPrompt,
            None if learned => PendingAction::EndPrompt,
            None => PendingAction::Wait,
        }
    }
}

/// Answers the pager while a table is being read, and only then: the same
/// prompt from a help page the player is reading is theirs to answer.
#[derive(Debug, Default)]
pub struct TablePager {
    reading: bool,
    /// The pending line last answered, so it's answered once.
    answered: Option<u64>,
}

impl TablePager {
    pub fn begin_table_read(&mut self) {
        self.reading = true;
    }

    pub fn end_table_read(&mut self) {
        self.reading = false;
    }

    pub fn is_reading(&self) -> bool {
        self.reading
    }

    /// Whether to answer the pager prompt pending on line `number`.
    fn answer(&mut self, number: u64) -> bool {
        if !self.reading || self.answered == Some(number) {
            return false;
        }
        self.answered = Some(number);
        true
    }
}
