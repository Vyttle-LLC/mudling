//! Auto-login: answering the login prompts a plugin's Connection point names.

use crate::plugin::PluginHost;
use crate::plugin::points::{LoginPrompt, LoginReply};
use std::fmt;

/// What auto-login sends. It goes only to the connection.
#[derive(Clone)]
pub struct Credentials {
    pub name: String,
    pub password: String,
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// The login prompts still to come. Each is answered once: a server asking
/// for the password twice has refused it, and asking again would only risk
/// a lockout.
pub(super) struct Login {
    prompts: Vec<LoginPrompt>,
    credentials: Credentials,
}

impl Login {
    /// `None` when no enabled plugin names a login prompt. With no password
    /// saved only the name is answered: the player types the rest.
    pub(super) fn new(host: &PluginHost, credentials: Credentials) -> Option<Login> {
        let prompts: Vec<LoginPrompt> = host
            .declared(|points| points.connection.as_ref())
            .flat_map(|(_, connection)| connection.logins.iter().cloned())
            .filter(|prompt| !credentials.password.is_empty() || prompt.send == LoginReply::Name)
            .collect();
        (!prompts.is_empty()).then_some(Login {
            prompts,
            credentials,
        })
    }

    /// The line to send if `text`, a finished line or the pending text, is a
    /// prompt still unanswered.
    pub(super) fn answer(&mut self, text: &str) -> Option<&str> {
        let index = self
            .prompts
            .iter()
            .position(|prompt| prompt.pattern.is_match(text))?;
        Some(match self.prompts.remove(index).send {
            LoginReply::Name => &self.credentials.name,
            LoginReply::Password => &self.credentials.password,
            LoginReply::Blank => "",
        })
    }

    pub(super) fn is_done(&self) -> bool {
        self.prompts.is_empty()
    }
}
