//! The telnet layer's public face: what comes out of the bytes.

use mudling_core::telnet::command::{DO, DONT, GA, IAC, SB, SE, WILL, WONT};
use mudling_core::telnet::option::{ECHO, GMCP};
use mudling_core::telnet::{Bytes, Event, Parser, encode_line, gmcp};

const MCCP2: u8 = 86;
/// An option this client doesn't take up.
const TERMINAL_TYPE: u8 = 24;

fn read(chunks: &[&[u8]]) -> (Vec<Event>, Vec<u8>) {
    let mut parser = Parser::new();
    let mut events = Vec::new();
    for chunk in chunks {
        parser
            .feed(Bytes::copy_from_slice(chunk), &mut events)
            .unwrap();
    }
    (events, parser.take_output())
}

fn data(text: &[u8]) -> Event {
    Event::Data(Bytes::copy_from_slice(text))
}

fn gmcp_event(package: &str, payload: &str) -> Event {
    Event::Gmcp {
        package: package.to_owned(),
        data: payload.to_owned(),
    }
}

#[test]
fn an_escaped_iac_is_a_literal_byte() {
    let (events, _) = read(&[&[65, IAC, IAC, 66]]);
    let text: Vec<u8> = events
        .iter()
        .flat_map(|event| match event {
            Event::Data(text) => text.to_vec(),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(text, [65, IAC, 66]);
}

#[test]
fn a_negotiation_comes_after_the_text_before_it() {
    let (events, output) = read(&[&[72, 105, IAC, WILL, GMCP, 33]]);
    assert_eq!(events, [data(b"Hi"), Event::GmcpEnabled, data(b"!")]);
    assert_eq!(output, [IAC, DO, GMCP]);
}

#[test]
fn a_gmcp_message_is_read_whole_however_it_arrives() {
    let mut frame = vec![IAC, SB, GMCP];
    frame.extend_from_slice(br#"Room.Info {"num":1}"#);
    frame.extend_from_slice(&[IAC, SE]);
    let expected = [gmcp_event("Room.Info", r#"{"num":1}"#)];
    assert_eq!(read(&[&frame]).0, expected);
    let bytes: Vec<&[u8]> = frame.chunks(1).collect();
    assert_eq!(read(&bytes).0, expected);
}

#[test]
fn an_escaped_iac_inside_a_message_is_unescaped() {
    let mut frame = vec![IAC, SB, GMCP];
    frame.extend_from_slice(b"Note \"a");
    frame.extend_from_slice(&[IAC, IAC, IAC, SE]);
    let (events, _) = read(&[&frame]);
    assert_eq!(events.len(), 1, "{events:?}");
    assert!(matches!(&events[0], Event::Gmcp { package, .. } if package == "Note"));
}

#[test]
fn go_ahead_ends_the_text_before_it() {
    assert_eq!(read(&[&[72, IAC, GA]]).0, [data(b"H"), Event::PromptEnd]);
}

#[test]
fn offers_are_taken_up_or_refused() {
    let (events, output) = read(&[&[IAC, WILL, TERMINAL_TYPE, IAC, DO, TERMINAL_TYPE]]);
    assert_eq!(events, []);
    assert_eq!(output, [IAC, DONT, TERMINAL_TYPE, IAC, WONT, TERMINAL_TYPE]);
    assert_eq!(read(&[&[IAC, WILL, MCCP2]]).1, [IAC, DO, MCCP2]);
}

#[test]
fn acknowledgements_need_no_reply() {
    assert_eq!(read(&[&[IAC, WONT, GMCP, IAC, DONT, ECHO]]).1, [0u8; 0]);
}

#[test]
fn a_message_without_a_payload_has_no_data() {
    let mut frame = vec![IAC, SB, GMCP];
    frame.extend_from_slice(b"Core.Ping");
    frame.extend_from_slice(&[IAC, SE]);
    assert_eq!(read(&[&frame]).0, [gmcp_event("Core.Ping", "")]);
}

#[test]
fn a_payload_is_json() {
    let room: serde_json::Value = gmcp::from_json(r#"{"num":42}"#).unwrap();
    assert_eq!(room["num"], 42);
}

#[test]
fn a_message_goes_out_in_a_gmcp_frame() {
    let mut out = Vec::new();
    gmcp::encode("Core.Ping", "", &mut out);
    let mut frame = vec![IAC, SB, GMCP];
    frame.extend_from_slice(b"Core.Ping");
    frame.extend_from_slice(&[IAC, SE]);
    assert_eq!(out, frame);

    out.clear();
    encode_line(b"look", &mut out);
    assert_eq!(out, b"look\r\n");
}
