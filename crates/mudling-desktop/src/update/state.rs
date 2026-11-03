//! What the app knows about updates, and what the views show of it.

use super::{Checked, Eligibility, Failure, Installed, Staged};
use mudling_core::persist::Timestamp;
use serde::{Deserialize, Serialize};

/// How often a running app checks again.
pub const CHECK_EVERY_MILLIS: i64 = 24 * 60 * 60 * 1000;
const SOURCE_BUILD: &str = "Built from source, so it doesn't update itself.";
/// Measured runs and checks keep no settings, so they never check.
const NOT_THIS_WINDOW: &str = "This window doesn't check for updates.";

/// The player's update choices, kept across launches.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateChoices {
    /// "Check for updates", on until the player turns it off.
    pub check: bool,
    pub last_checked: Option<Timestamp>,
    /// The release whose card the player put off with Later.
    pub later: Option<String>,
    /// A release that couldn't be downloaded, verified or put in, which
    /// checks pass over until a newer one, or Check now.
    pub skipped: Option<String>,
}

impl Default for UpdateChoices {
    fn default() -> UpdateChoices {
        UpdateChoices {
            check: true,
            last_checked: None,
            later: None,
            skipped: None,
        }
    }
}

pub struct Updates {
    eligibility: Eligibility,
    choices: UpdateChoices,
    staged: Option<Staged>,
    checking: bool,
    /// What the last check ran into before it found a release.
    problem: Option<String>,
    /// A release that couldn't be readied or put in, said once.
    failure: Option<Failure>,
}

impl Updates {
    /// `failed` is what the last exit's swap ran into, if it did.
    pub fn new(
        eligibility: Eligibility,
        choices: UpdateChoices,
        failed: Option<Failure>,
    ) -> Updates {
        let mut updates = Updates {
            eligibility,
            choices,
            staged: None,
            checking: false,
            problem: None,
            failure: None,
        };
        if let Some(failure) = failed {
            updates.failed(failure);
        }
        updates
    }

    pub fn installed(&self) -> Option<&Installed> {
        match &self.eligibility {
            Eligibility::Updates(installed) => Some(installed),
            _ => None,
        }
    }

    pub fn choices(&self) -> &UpdateChoices {
        &self.choices
    }

    pub fn staged(&self) -> Option<&Staged> {
        self.staged.as_ref()
    }

    /// Whether the daily check is due at `now`.
    pub fn due(&self, now: Timestamp) -> bool {
        self.choices.check
            && (self.choices.last_checked).is_none_or(|at| now.0 - at.0 >= CHECK_EVERY_MILLIS)
    }

    /// Starts a check, unless one is running, one is ready, or this copy
    /// can't update. Returns what to check and which release to pass over: a
    /// `manual` check tries a skipped one again.
    pub fn begin_check(&mut self, manual: bool) -> Option<(Installed, Option<String>)> {
        if self.checking || self.staged.is_some() {
            return None;
        }
        let installed = self.installed()?.clone();
        self.checking = true;
        let skip = self.choices.skipped.clone().filter(|_| !manual);
        Some((installed, skip))
    }

    pub fn finish_check(&mut self, checked: Result<Checked, Failure>, now: Timestamp) {
        self.checking = false;
        self.choices.last_checked = Some(now);
        self.problem = None;
        match checked {
            Ok(Checked::Current) => {}
            Ok(Checked::Ready(staged)) => {
                self.failure = None;
                self.staged = Some(staged);
            }
            Err(failure) if failure.version.is_some() => self.failed(failure),
            Err(failure) => self.problem = Some(failure.reason),
        }
    }

    fn failed(&mut self, failure: Failure) {
        self.choices.skipped = failure.version.clone();
        self.failure = Some(failure);
    }

    pub fn set_checking_on(&mut self, on: bool) {
        self.choices.check = on;
    }

    /// Hides the card until a newer release is ready; the dot stays.
    pub fn later(&mut self) {
        self.choices.later = self.staged.as_ref().map(|staged| staged.version.clone());
    }

    pub fn dismiss_failure(&mut self) {
        self.failure = None;
    }

    /// The update, for the app to put in as it exits.
    pub fn take_staged(&mut self) -> Option<Staged> {
        self.staged.take()
    }

    pub fn status(&self) -> UpdateStatus {
        let current = match self.installed() {
            Some(installed) => installed.version.clone(),
            None => UpdateStatus::default().current,
        };
        let cant = match &self.eligibility {
            Eligibility::Updates(_) => None,
            Eligibility::SourceBuild => Some(SOURCE_BUILD),
            Eligibility::CantReplace(why) => Some(*why),
        };
        let ready = self.staged.as_ref().map(|staged| Ready {
            version: staged.version.clone(),
            summary: staged.summary.clone(),
            page: staged.page.clone(),
            put_off: self.choices.later.as_ref() == Some(&staged.version),
        });
        let failure = self.failure.as_ref().map(|failure| {
            let version = failure.version.as_deref().unwrap_or_default();
            (
                format!("MUDling {version} didn't go in"),
                failure.reason.clone(),
            )
        });
        UpdateStatus {
            current,
            cant,
            check_on: self.choices.check,
            checking: self.checking,
            last_checked: self.choices.last_checked,
            problem: self.problem.clone(),
            ready,
            failure,
        }
    }
}

/// What the title bar, Settings and the empty state show of updates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateStatus {
    /// The version running, as Settings says it.
    pub current: String,
    /// Why this copy doesn't update itself, if it doesn't.
    pub cant: Option<&'static str>,
    pub check_on: bool,
    pub checking: bool,
    pub last_checked: Option<Timestamp>,
    /// What the last check ran into.
    pub problem: Option<String>,
    pub ready: Option<Ready>,
    /// A release that didn't go in, and why: its title and reason.
    pub failure: Option<(String, String)>,
}

impl Default for UpdateStatus {
    fn default() -> UpdateStatus {
        let (current, cant) = match mudling_core::RELEASE_VERSION {
            Some(release) => (release.to_owned(), NOT_THIS_WINDOW),
            None => (format!("{} dev", env!("CARGO_PKG_VERSION")), SOURCE_BUILD),
        };
        UpdateStatus {
            current,
            cant: Some(cant),
            check_on: true,
            checking: false,
            last_checked: None,
            problem: None,
            ready: None,
            failure: None,
        }
    }
}

impl UpdateStatus {
    /// The release that's ready, if its card shows: Later hides it until a
    /// newer one.
    pub fn card(&self) -> Option<&Ready> {
        self.ready.as_ref().filter(|ready| !ready.put_off)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ready {
    pub version: String,
    pub summary: Vec<String>,
    /// The release's page on GitHub.
    pub page: String,
    /// The player chose Later for this release.
    pub put_off: bool,
}
