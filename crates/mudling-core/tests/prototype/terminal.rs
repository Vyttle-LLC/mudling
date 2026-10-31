//! What the chat shows: the terminal store, through the engine.

use crate::driven::Driven;
use mudling_core::LineKind;
use mudling_core::persist::Timestamp;
use mudling_core::store::{TerminalState, Versioned};
use mudling_core::telnet::command::{GA, IAC};

/// Learns `<#hp> ` as the prompt, then the next `Char.Vitals` is due.
fn learned() -> Driven {
    let mut driven = Driven::new();
    driven.gmcp(r#"Char.Vitals {"hp": 20}"#);
    driven.say(b"<20hp> ");
    driven.say(&[IAC, GA]);
    driven
}

#[test]
fn a_prompt_that_was_never_learned_stays_visible() {
    let mut driven = Driven::new();
    driven.say(b"A stag stands here.\r\n<30hp> ");
    driven.engine.quiet(Timestamp(0));
    driven.engine.settle(Timestamp(0));
    assert_eq!(driven.terminal(), ["A stag stands here.", "<30hp> "]);
}

#[test]
fn a_run_of_blank_lines_shows_as_one() {
    let mut driven = Driven::new();
    driven.say(b"a\n\n\n\nb\n");
    assert_eq!(driven.terminal(), ["a", "", "b"]);
}

#[test]
fn blank_lines_around_a_hidden_prompt_show_as_one() {
    let mut driven = learned();
    driven.say(b"a\r\n\r\n");
    driven.gmcp(r#"Char.Vitals {"hp": 19}"#);
    driven.say(b"<19hp> \r\n\r\nb\r\n");
    assert_eq!(driven.terminal(), ["a", "", "b"]);
}

#[test]
fn a_blank_line_follows_a_command() {
    let mut driven = Driven::new();
    driven.say(b"\r\n");
    driven.send("look");
    driven.say(b"\r\nA road.\r\n");
    assert_eq!(driven.terminal(), ["look", "", "A road."]);
}

/// Text the server hasn't ended came before what the player sends, whether
/// or not it had gone quiet yet.
#[test]
fn a_command_shows_after_the_line_in_progress() {
    let mut driven = Driven::new();
    driven.say(b"Your choice: ");
    driven.send("look");
    assert_eq!(driven.terminal(), ["Your choice: ", "look"]);
    let kinds = driven.hub.read(|terminal: &Versioned<TerminalState>| {
        let lines = terminal.value().lines().iter();
        lines.map(|line| line.kind()).collect::<Vec<_>>()
    });
    assert_eq!(kinds, [LineKind::Server, LineKind::Command]);

    // Silence ends the line; what the chat shows already isn't shown again.
    driven.engine.quiet(Timestamp(0));
    driven.engine.settle(Timestamp(0));
    assert_eq!(driven.terminal(), ["Your choice: ", "look"]);
}

#[test]
fn the_rest_of_a_line_shows_after_the_command_that_split_it() {
    let mut driven = Driven::new();
    driven.say(b"A long ");
    driven.send("look");
    driven.say(b"line");
    driven.send("west");
    driven.say(b" ends.\r\nNext.\r\n");
    assert_eq!(
        driven.terminal(),
        ["A long ", "look", "line", "west", " ends.", "Next."]
    );
}

#[test]
fn a_reply_comes_after_the_command_it_answers() {
    let mut driven = learned();
    driven.gmcp(r#"Char.Vitals {"hp": 19}"#);
    driven.say(b"<19hp> ");
    driven.send("save");
    driven.say(b"Saved.");
    driven.say(b"\n\r");
    assert_eq!(driven.terminal(), ["save", "Saved."]);
}
