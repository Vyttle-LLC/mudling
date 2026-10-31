//! Auto-login through the engine, on a game's Connection point.

use crate::driven::Driven;
use mudling_core::plugin::Plugin;
use mudling_core::session::Credentials;

const NAME_AND_PASSWORD: &str = "[[login]]\npattern = '''(?i)name'''\nsend = \"name\"\n\
     [[login]]\npattern = '''(?i)password'''\nsend = \"password\"\n";
const BANNER: &str = "[[login]]\npattern = '''press <RETURN>'''\nsend = \"blank\"\n";

fn game(logins: String) -> Plugin {
    let manifest = "id = \"gate\"\nname = \"Gate\"\nversion = \"1.0.0\"\napi = 1\n\
         license = \"MIT\"\nlayer = \"game\"\n[servers]\nhosts = [\"mud.example.org\"]\n\
         [points]\nconnection = \"connection.toml\"\n";
    Plugin::from_files(|file| match file {
        "plugin.toml" => Ok(manifest.to_owned()),
        "connection.toml" => Ok(logins.clone()),
        _ => Err(std::io::Error::other(file.to_owned())),
    })
    .unwrap_or_else(|error| panic!("{error}"))
}

fn logged_in(logins: String, password: &str) -> Driven {
    let credentials = Credentials {
        name: "Testchar".to_owned(),
        password: password.to_owned(),
    };
    Driven::with(vec![game(logins)], Some(credentials))
}

#[test]
fn the_name_then_the_password_are_answered_once_each() {
    let logins = format!("{NAME_AND_PASSWORD}{BANNER}");
    let mut driven = logged_in(logins, "hunter2");
    driven.say(b"Welcome!\r\n");
    assert_eq!(driven.sent(), [] as [String; 0]);
    driven.say(b"What name shall you be known by? ");
    assert_eq!(driven.sent(), ["Testchar"]);
    driven.say(b"\r\nPassword: ");
    assert_eq!(driven.sent(), ["hunter2"]);
    driven.say(b"\r\nPassword: ");
    assert_eq!(
        driven.sent(),
        [] as [String; 0],
        "a second ask is a refusal"
    );
}

#[test]
fn a_banner_is_answered_with_a_blank_line() {
    let logins = format!("{NAME_AND_PASSWORD}{BANNER}");
    let mut driven = logged_in(logins, "pw");
    driven.say(b"What name shall you be known by? ");
    driven.say(b"\r\nExisting Character.\r\nYour Password: ");
    driven.sent();
    driven.say(b"\r\n/ Typing COMMANDS gives you a list /\r\n");
    assert_eq!(driven.sent(), [] as [String; 0]);
    driven.say(b"When you have read this, please press <RETURN> to continue ---->");
    assert_eq!(driven.sent(), [""]);
}

#[test]
fn a_character_with_no_saved_password_stops_after_the_name() {
    let logins = format!("{NAME_AND_PASSWORD}{BANNER}");
    let mut driven = logged_in(logins, "");
    driven.say(b"By what name do you wish to be known? ");
    assert_eq!(driven.sent(), ["Testchar"]);
    driven.say(b"\r\nPassword: ");
    assert_eq!(driven.sent(), [] as [String; 0], "the player types it");
    driven.say(b"\r\nplease press <RETURN> to continue ---->");
    assert_eq!(driven.sent(), [] as [String; 0]);
}

#[test]
fn a_games_own_prompts_match() {
    let logins = "[[login]]\npattern = '''(?i)who are you'''\nsend = \"name\"\n\
         [[login]]\npattern = '''(?i)secret word'''\nsend = \"password\"\n";
    let mut driven = logged_in(logins.to_owned(), "pw");
    driven.say(b"Who are you? ");
    assert_eq!(driven.sent(), ["Testchar"]);
    driven.say(b"\r\nAnd your secret word? ");
    assert_eq!(driven.sent(), ["pw"]);
}
