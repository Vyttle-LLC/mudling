//! What the plugin reads in Aardwolf's text: the prompt, the exits line and
//! the moves the game refuses.

use mudling_core::line::LinePipeline;
use mudling_core::plugin::{PendingKind, PluginEvent, PluginHost};
use mudling_plugin_aardwolf::{events, load};

/// What the host makes of `text` sent without a line end.
fn pending(host: &mut PluginHost, text: &str) -> Option<PendingKind> {
    let mut pipeline = LinePipeline::default();
    pipeline.push(text.as_bytes(), &mut Vec::new());
    host.read_pending(&pipeline.pending().unwrap())
}

fn named<'a>(events: &'a [PluginEvent], name: &str) -> Vec<&'a PluginEvent> {
    (events.iter())
        .filter(|event| &*event.name == name)
        .collect()
}

#[test]
fn a_whole_prompt_is_recognized_and_half_of_one_is_not() {
    let mut host = PluginHost::new([load()]).unwrap();
    for prompt in [
        "<400/420hp 610/610mn 515/530mv 980tnl>",
        "<400/420hp 610/610mn 515/530mv 980tnl> ",
        "10hp 10mn 10mv qt0>",
        "[-12/420hp 0/610mn 3/530mv]>",
    ] {
        assert_eq!(
            pending(&mut host, prompt),
            Some(PendingKind::Prompt),
            "{prompt}"
        );
    }
    // A network read can split a prompt; the half isn't one.
    for half in ["<400/420hp 610/6", "<400/420hp 610/610mn 515/530mv", "10hp"] {
        assert_eq!(pending(&mut host, half), None, "{half}");
    }
}

#[test]
fn the_exits_line_lists_closed_doors_in_parentheses() {
    let events = events(include_str!("sample.txt"));
    let exits: Vec<Option<&str>> = (named(&events, "map.exits").into_iter())
        .map(|event| event.capture("exits"))
        .collect();
    assert_eq!(exits, [Some("north east (south)"), Some("south")]);
}

#[test]
fn the_exits_line_is_read_after_a_prompt_glued_in_front() {
    let events = events("<1hp 2mn 3mv>[ Exits: up down ]\n");
    let exits = named(&events, "map.exits");
    assert_eq!(exits.len(), 1);
    assert_eq!(exits[0].capture("exits"), Some("up down"));
}

#[test]
fn a_refused_move_is_a_failed_move() {
    let refusals = "Alas, you cannot go that way.\n\
        There is no exit in that direction.\n\
        You need to use a boat, fly, or swim underwater to go there.\n\
        You must be standing first.\n\
        You are regaining balance and are unable to move.\n\
        Wren says 'Alas, you cannot go that way.'\n";
    let events = events(refusals);
    assert_eq!(named(&events, "map.failed_move").len(), 5);
}
