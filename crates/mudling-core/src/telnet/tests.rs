use super::command::*;
use super::*;
use flate2::Compression;
use flate2::write::ZlibEncoder;
use std::io::Write;
use std::time::Instant;

const NOP: u8 = 241;

/// A synthetic session opening: every kind of sequence the parser handles.
fn fixture() -> Vec<u8> {
    let mut stream = Vec::new();
    stream.extend_from_slice(&[IAC, WILL, option::GMCP, IAC, WILL, option::EOR]);
    stream.extend_from_slice(&[IAC, DO, option::NAWS, IAC, WILL, 24, IAC, DO, 24]);
    stream.extend_from_slice(&[IAC, SB, option::MSSP, MSSP_VAR]);
    stream.extend_from_slice(b"PLAYERS");
    stream.push(MSSP_VAL);
    stream.extend_from_slice(b"3");
    stream.push(MSSP_VAR);
    stream.extend_from_slice(b"NAME");
    stream.push(MSSP_VAL);
    stream.extend_from_slice(b"Test Realm");
    stream.extend_from_slice(&[IAC, SE]);
    stream.extend_from_slice(b"\x1b[1;32mWelcome, traveller.\x1b[0m\r\nA byte: ");
    stream.extend_from_slice(&[IAC, IAC]);
    stream.extend_from_slice(b"\r\nWhat is your name? ");
    stream.extend_from_slice(&[IAC, GA, IAC, WILL, option::ECHO]);
    stream.extend_from_slice(b"Password: ");
    stream.extend_from_slice(&[IAC, EOR, IAC, WONT, option::ECHO, IAC, NOP]);
    stream.extend_from_slice(&[IAC, SB, option::GMCP]);
    stream.extend_from_slice(b"Char.Vitals {\"hp\":10,\"bar\":\"");
    stream.extend_from_slice(&[IAC, IAC]);
    stream.extend_from_slice(b"\"}");
    stream.extend_from_slice(&[IAC, SE]);
    stream.extend_from_slice(&[IAC, SB, option::GMCP]);
    stream.extend_from_slice(b"Core.Ping");
    stream.extend_from_slice(&[IAC, SE]);
    stream.extend_from_slice(b"You are standing in a field.\r\n");
    stream
}

fn fixture_events() -> Vec<Event> {
    vec![
        Event::GmcpEnabled,
        Event::Mssp("Test Realm".to_owned()),
        Event::Data(Bytes::from_static(
            b"\x1b[1;32mWelcome, traveller.\x1b[0m\r\nA byte: \xFF\r\nWhat is your name? ",
        )),
        Event::PromptEnd,
        Event::Echo(false),
        text("Password: "),
        Event::PromptEnd,
        Event::Echo(true),
        Event::Gmcp {
            package: "Char.Vitals".to_owned(),
            data: "{\"hp\":10,\"bar\":\"\u{FFFD}\"}".to_owned(),
        },
        Event::Gmcp {
            package: "Core.Ping".to_owned(),
            data: String::new(),
        },
        text("You are standing in a field.\r\n"),
    ]
}

fn fixture_output() -> Vec<u8> {
    vec![
        IAC,
        DO,
        option::GMCP,
        IAC,
        DO,
        option::EOR,
        IAC,
        WONT,
        option::NAWS,
        IAC,
        DONT,
        24,
        IAC,
        WONT,
        24,
        IAC,
        DO,
        option::ECHO,
        IAC,
        DONT,
        option::ECHO,
    ]
}

fn text(text: &str) -> Event {
    Event::Data(Bytes::copy_from_slice(text.as_bytes()))
}

/// Feeds `chunks` in order and returns the events, with neighbouring text
/// merged so that differently split streams compare equal, and the output.
fn run(chunks: &[&[u8]]) -> (Vec<Event>, Vec<u8>) {
    let mut parser = Parser::new();
    let mut events = Vec::new();
    for chunk in chunks {
        parser
            .feed(Bytes::copy_from_slice(chunk), &mut events)
            .unwrap();
    }
    (merge_text(events), parser.take_output())
}

fn merge_text(events: Vec<Event>) -> Vec<Event> {
    let mut merged: Vec<Event> = Vec::new();
    for event in events {
        match (merged.last_mut(), event) {
            (Some(Event::Data(previous)), Event::Data(next)) => {
                *previous = Bytes::from([previous.as_ref(), next.as_ref()].concat());
            }
            (_, event) => merged.push(event),
        }
    }
    merged
}

#[test]
fn parses_the_fixture_in_one_chunk() {
    let stream = fixture();
    assert_eq!(run(&[&stream]), (fixture_events(), fixture_output()));
}

#[test]
fn every_split_point_gives_the_same_result() {
    let stream = fixture();
    let expected = (fixture_events(), fixture_output());
    for split in 0..=stream.len() {
        let (head, tail) = stream.split_at(split);
        assert_eq!(run(&[head, tail]), expected, "split at byte {split}");
    }
}

#[test]
fn one_byte_at_a_time_gives_the_same_result() {
    let stream = fixture();
    let chunks: Vec<&[u8]> = stream.chunks(1).collect();
    assert_eq!(run(&chunks), (fixture_events(), fixture_output()));
}

#[test]
fn text_is_sliced_from_the_chunk_without_copying() {
    let chunk = Bytes::from_static(b"north\r\n\xFF\xF9south\r\n");
    let mut events = Vec::new();
    Parser::new().feed(chunk.clone(), &mut events).unwrap();
    let Event::Data(north) = &events[0] else {
        panic!("expected text")
    };
    assert_eq!(north.as_ptr(), chunk.as_ptr());
    let Event::Data(south) = &events[2] else {
        panic!("expected text")
    };
    assert_eq!(south.as_ptr(), chunk[9..].as_ptr());
}

#[test]
fn negotiation_replies_only_to_changes() {
    let (events, output) = run(&[&[
        IAC,
        WILL,
        option::ECHO,
        IAC,
        WILL,
        option::ECHO,
        IAC,
        WONT,
        option::ECHO,
        IAC,
        WONT,
        option::ECHO,
    ]]);
    assert_eq!(events, [Event::Echo(false), Event::Echo(true)]);
    assert_eq!(output, [IAC, DO, option::ECHO, IAC, DONT, option::ECHO]);
}

#[test]
fn a_dont_needs_no_answer() {
    assert_eq!(run(&[&[IAC, DONT, option::ECHO]]), (Vec::new(), Vec::new()));
}

#[test]
fn mssp_surfaces_only_the_name() {
    let mut stream = vec![IAC, SB, option::MSSP, MSSP_VAR];
    stream.extend_from_slice(b"UPTIME");
    stream.push(MSSP_VAL);
    stream.extend_from_slice(b"123");
    stream.extend_from_slice(&[IAC, SE, IAC, SB, option::MSSP, MSSP_VAR]);
    stream.extend_from_slice(b"CODEBASE");
    stream.push(MSSP_VAL);
    stream.extend_from_slice(b"Tiny");
    stream.extend_from_slice(&[IAC, SE]);
    assert_eq!(run(&[&stream]).0, []);
}

#[test]
fn encode_line_doubles_iac_and_ends_with_crlf() {
    let mut out = Vec::new();
    encode_line(b"say \xFF\xFF!", &mut out);
    assert_eq!(out, b"say \xFF\xFF\xFF\xFF!\r\n");
}

#[test]
fn gmcp_round_trips_through_the_parser() {
    let mut stream = Vec::new();
    gmcp::encode_hello("MUDling", "1.0", &mut stream);
    gmcp::encode_supports_set(&["Char 1", "Room 1"], &mut stream);
    assert_eq!(
        run(&[&stream]).0,
        [
            Event::Gmcp {
                package: "Core.Hello".to_owned(),
                data: r#"{"client":"MUDling","version":"1.0"}"#.to_owned()
            },
            Event::Gmcp {
                package: "Core.Supports.Set".to_owned(),
                data: r#"["Char 1","Room 1"]"#.to_owned()
            },
        ]
    );
}

#[test]
fn a_32_kb_gmcp_message_parses_in_linear_time() {
    // Doubling the size should about double the time; quadratic collection
    // (the prototype's) would quadruple it. Small chunks make the split count
    // as large as possible.
    let seconds = |size: usize| {
        let mut stream = vec![IAC, SB, option::GMCP];
        stream.extend_from_slice(b"Char.Items.List ");
        stream.resize(stream.len() + size, b'x');
        stream.extend_from_slice(&[IAC, SE]);
        let chunks: Vec<Bytes> = stream.chunks(256).map(Bytes::copy_from_slice).collect();
        let started = Instant::now();
        for _ in 0..20 {
            let mut parser = Parser::new();
            let mut events = Vec::new();
            for chunk in &chunks {
                parser.feed(chunk.clone(), &mut events).unwrap();
            }
            assert_eq!(events.len(), 1);
        }
        started.elapsed().as_secs_f64()
    };
    let small = seconds(32 * 1024);
    let large = seconds(32 * 32 * 1024);
    assert!(
        large < small * 32.0 * 4.0,
        "32× the size took {:.0}× the time",
        large / small
    );
}

fn compress(plain: &[u8]) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(plain).unwrap();
    encoder.finish().unwrap()
}

/// Plain text, compression starting mid-chunk, the fixture compressed, the
/// stream ending, then plain text again.
fn compressed_fixture() -> Vec<u8> {
    let mut stream = b"Plain before. ".to_vec();
    stream.extend_from_slice(&[
        IAC,
        WILL,
        option::COMPRESS2,
        IAC,
        SB,
        option::COMPRESS2,
        IAC,
        SE,
    ]);
    stream.extend_from_slice(&compress(&fixture()));
    stream.extend_from_slice(b"Plain after.");
    stream
}

fn compressed_fixture_events() -> Vec<Event> {
    let mut events = vec![text("Plain before. ")];
    events.extend(fixture_events());
    merge_text(events.into_iter().chain([text("Plain after.")]).collect())
}

fn compressed_fixture_output() -> Vec<u8> {
    let mut output = vec![IAC, DO, option::COMPRESS2];
    output.extend(fixture_output());
    output
}

#[test]
fn mccp2_starts_mid_stream_and_ends_cleanly() {
    let stream = compressed_fixture();
    let expected = (compressed_fixture_events(), compressed_fixture_output());
    for split in 0..=stream.len() {
        let (head, tail) = stream.split_at(split);
        assert_eq!(run(&[head, tail]), expected, "split at byte {split}");
    }
    let chunks: Vec<&[u8]> = stream.chunks(1).collect();
    assert_eq!(run(&chunks), expected);
}

#[test]
fn mccp2_output_larger_than_a_block_is_all_delivered() {
    let plain = vec![b'a'; 100_000];
    let mut stream = vec![
        IAC,
        WILL,
        option::COMPRESS2,
        IAC,
        SB,
        option::COMPRESS2,
        IAC,
        SE,
    ];
    stream.extend_from_slice(&compress(&plain));
    let (events, _) = run(&[&stream]);
    assert_eq!(events, [Event::Data(Bytes::from(plain))]);
}

#[test]
fn a_refused_compression_start_is_ignored() {
    let (events, _) = run(&[&[IAC, SB, option::COMPRESS2, IAC, SE], b"plain"]);
    assert_eq!(events, [text("plain")]);
}

#[test]
fn corrupt_compressed_data_is_an_error_that_ends_compression() {
    let mut parser = Parser::new();
    let mut events = Vec::new();
    let mut stream = vec![
        IAC,
        WILL,
        option::COMPRESS2,
        IAC,
        SB,
        option::COMPRESS2,
        IAC,
        SE,
    ];
    stream.extend_from_slice(b"this is not zlib");
    assert!(parser.feed(Bytes::from(stream), &mut events).is_err());
    parser
        .feed(Bytes::from_static(b"plain"), &mut events)
        .unwrap();
    assert_eq!(events.last(), Some(&text("plain")));
}
