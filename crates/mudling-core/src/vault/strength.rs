//! The strength check on new master passwords. The vault can sit in a git repo
//! where anyone with read access can guess offline, so this asks for zxcvbn's
//! top score, which estimates at least 10^10 guesses.

use super::error::Error;
use super::secret::Secret;
use zxcvbn::{Score, zxcvbn};

/// Words a player's guess shouldn't lean on.
const PRODUCT_WORDS: [&str; 3] = ["mudling", "vault", "password"];

/// Refuses a password that would be too easy to guess, with zxcvbn's reason
/// when it has one. Anything using this to give live feedback should keep in
/// mind that zxcvbn copies the password internally without wiping it.
pub fn check(password: &Secret) -> Result<(), Error> {
    let estimate = zxcvbn(password.expose(), &PRODUCT_WORDS);
    if estimate.score() == Score::Four {
        return Ok(());
    }
    let warning = estimate
        .feedback()
        .and_then(|feedback| feedback.warning())
        .map(|warning| warning.to_string());
    Err(Error::WeakPassword { warning })
}
