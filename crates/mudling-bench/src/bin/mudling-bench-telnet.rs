//! `mudling-bench-telnet`: measures the telnet parser on synthetic traffic.
//!
//! ```text
//! mudling-bench-telnet [--check] [--json <report.json>]
//! ```
//!
//! The traffic is shaped like a real session: colored room text and combat,
//! a prompt with GA after each burst, GMCP vitals and room info, and now and
//! then a large GMCP item list. It is fed in 4 KB chunks, plain and through
//! MCCP2, timing each chunk. A large GMCP message is also parsed at 32 KB and
//! 1 MB, to show the cost per byte doesn't grow with its size.
//!
//! The telnet stage shares the 20 µs per line budget (PRD §6) with the line
//! pipeline and every plugin, so `--check` holds it to a tenth of that: a
//! 4 KB chunk, about 50 lines, within 100 µs at p99.

use flate2::Compression;
use flate2::write::ZlibEncoder;
use mudling_core::stats::Summary;
use mudling_core::telnet::{Bytes, Parser};
use serde::Serialize;
use std::io::Write;
use std::process::ExitCode;
use std::time::{Duration, Instant};

const IAC: u8 = 255;
const SB: u8 = 250;
const SE: u8 = 240;
const GA: u8 = 249;
const WILL: u8 = 251;
const GMCP: u8 = 201;
const COMPRESS2: u8 = 86;

const CHUNK: usize = 4 * 1024;
const BURSTS: usize = 20_000;
const CHUNK_BUDGET_US: f64 = 100.0;

#[derive(Serialize)]
struct Run {
    wire_mb: f64,
    text_mb: f64,
    chunks: usize,
    wire_mb_per_s: f64,
    text_mb_per_s: f64,
    per_chunk: Summary,
}

#[derive(Serialize)]
struct Report {
    plain: Run,
    mccp2: Run,
    gmcp_32kb_ns_per_byte: f64,
    gmcp_1mb_ns_per_byte: f64,
    chunk_budget_us: f64,
    within_budget: bool,
}

fn main() -> ExitCode {
    let mut check = false;
    let mut json = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--check" => check = true,
            "--json" => json = args.next(),
            _ => {
                eprintln!("usage: mudling-bench-telnet [--check] [--json <report.json>]");
                return ExitCode::from(2);
            }
        }
    }

    let bursts: Vec<Vec<u8>> = (0..BURSTS).map(burst).collect();
    let plain: Vec<u8> = negotiation(false)
        .into_iter()
        .chain(bursts.concat())
        .collect();
    let text_bytes = plain.len();
    let plain = measure(&plain, text_bytes);
    let mccp2 = measure(&compressed(&bursts), text_bytes);
    let gmcp_32kb_ns_per_byte = gmcp_ns_per_byte(32 * 1024);
    let gmcp_1mb_ns_per_byte = gmcp_ns_per_byte(1024 * 1024);
    let within_budget =
        plain.per_chunk.p99_us <= CHUNK_BUDGET_US && mccp2.per_chunk.p99_us <= CHUNK_BUDGET_US;
    let report = Report {
        plain,
        mccp2,
        gmcp_32kb_ns_per_byte,
        gmcp_1mb_ns_per_byte,
        chunk_budget_us: CHUNK_BUDGET_US,
        within_budget,
    };

    print_report(&report);
    if let Some(path) = json {
        let text = serde_json::to_string_pretty(&report).expect("the report serializes");
        if let Err(error) = std::fs::write(&path, text) {
            eprintln!("couldn't write {path}: {error}");
            return ExitCode::from(2);
        }
    }
    if check && !report.within_budget {
        eprintln!("a budget was missed");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// One server write: what a move, a combat round or a tick sends.
fn burst(index: usize) -> Vec<u8> {
    let mut out = Vec::new();
    match index % 4 {
        0 => {
            out.extend_from_slice(
                format!("\x1b[1;36mA Winding Path {index}\x1b[0m\r\n").as_bytes(),
            );
            for line in 0..4 {
                out.extend_from_slice(
                    format!("  The path bends past old stones and tall grass, line {line} of the room.\r\n").as_bytes(),
                );
            }
            out.extend_from_slice(b"\x1b[0;32m[Exits: north east south]\x1b[0m\r\n");
            out.extend_from_slice(b"\x1b[1;33mA small brown rabbit is here.\x1b[0m\r\n");
            gmcp(
                &mut out,
                &format!(
                    "Room.Info {{\"num\":{index},\"name\":\"A Winding Path\",\"area\":\"The Fields\",\
                     \"environment\":\"field\",\"coords\":\"0,{index},0,0\",\"exits\":{{\"n\":{},\"e\":{},\"s\":{}}}}}",
                    index + 1,
                    index + 2,
                    index + 3
                ),
            );
        }
        1 | 2 => {
            for hit in 0..3 {
                out.extend_from_slice(
                    format!("Your slash \x1b[1;31mstrikes\x1b[0m the rabbit ({hit}). The rabbit squeaks.\r\n").as_bytes(),
                );
            }
        }
        _ => out.extend_from_slice(b"\x1b[0;35mThe wind picks up.\x1b[0m\r\n"),
    }
    if index.is_multiple_of(500) {
        let items: Vec<String> = (0..200)
            .map(|item| format!("{{\"id\":{item},\"name\":\"\x1b[1;34ma shiny thing {item}\x1b[0m\",\"attrib\":\"wt\"}}"))
            .collect();
        gmcp(
            &mut out,
            &format!("Char.Items.List {{\"items\":[{}]}}", items.join(",")),
        );
    }
    gmcp(
        &mut out,
        &format!(
            "Char.Vitals {{\"hp\":{},\"maxhp\":120,\"mana\":80,\"maxmana\":90}}",
            index % 120
        ),
    );
    out.extend_from_slice(format!("<{}hp 80m 90mv> ", index % 120).as_bytes());
    out.extend_from_slice(&[IAC, GA]);
    out
}

fn gmcp(out: &mut Vec<u8>, message: &str) {
    out.extend_from_slice(&[IAC, SB, GMCP]);
    out.extend_from_slice(message.as_bytes());
    out.extend_from_slice(&[IAC, SE]);
}

fn negotiation(compress: bool) -> Vec<u8> {
    let mut out = vec![IAC, WILL, GMCP];
    if compress {
        out.extend_from_slice(&[IAC, WILL, COMPRESS2, IAC, SB, COMPRESS2, IAC, SE]);
    }
    out
}

/// The bursts through MCCP2, flushed after each as a server does.
fn compressed(bursts: &[Vec<u8>]) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(negotiation(true), Compression::default());
    for burst in bursts {
        encoder.write_all(burst).expect("writing to a Vec succeeds");
        encoder.flush().expect("writing to a Vec succeeds");
    }
    encoder.finish().expect("writing to a Vec succeeds")
}

fn measure(stream: &[u8], text_bytes: usize) -> Run {
    let chunks: Vec<Bytes> = stream.chunks(CHUNK).map(Bytes::copy_from_slice).collect();
    let mut parser = Parser::new();
    let mut events = Vec::new();
    let mut times = Vec::with_capacity(chunks.len());
    for chunk in chunks {
        let started = Instant::now();
        parser
            .feed(chunk, &mut events)
            .expect("the synthetic stream is valid");
        times.push(started.elapsed());
        std::hint::black_box(&events);
        events.clear();
    }
    let total: Duration = times.iter().sum();
    let megabytes = |bytes: usize| bytes as f64 / (1024.0 * 1024.0);
    Run {
        wire_mb: megabytes(stream.len()),
        text_mb: megabytes(text_bytes),
        chunks: times.len(),
        wire_mb_per_s: megabytes(stream.len()) / total.as_secs_f64(),
        text_mb_per_s: megabytes(text_bytes) / total.as_secs_f64(),
        per_chunk: Summary::of(&times).expect("chunks were timed"),
    }
}

/// Parses one GMCP message of `size` bytes, arriving in 4 KB chunks.
fn gmcp_ns_per_byte(size: usize) -> f64 {
    let mut stream = Vec::new();
    gmcp(
        &mut stream,
        &format!("Char.Items.List {}", "x".repeat(size)),
    );
    let chunks: Vec<Bytes> = stream.chunks(CHUNK).map(Bytes::copy_from_slice).collect();
    let rounds = (64 * 1024 * 1024 / size).max(1);
    let started = Instant::now();
    for _ in 0..rounds {
        let mut parser = Parser::new();
        let mut events = Vec::new();
        for chunk in &chunks {
            parser
                .feed(chunk.clone(), &mut events)
                .expect("the stream is valid");
        }
        std::hint::black_box(events);
    }
    started.elapsed().as_secs_f64() * 1e9 / (rounds * stream.len()) as f64
}

fn print_report(report: &Report) {
    for (name, run) in [("plain", &report.plain), ("MCCP2", &report.mccp2)] {
        println!(
            "{name}: {:.1} MB on the wire ({:.1} MB of telnet) in {} chunks of 4 KB; {:.0} MB/s wire, {:.0} MB/s telnet",
            run.wire_mb, run.text_mb, run.chunks, run.wire_mb_per_s, run.text_mb_per_s
        );
        println!(
            "  per chunk: p50 {:.2} µs, p99 {:.2} µs, max {:.1} µs; budget p99 ≤ {} µs: {}",
            run.per_chunk.p50_us,
            run.per_chunk.p99_us,
            run.per_chunk.max_us,
            report.chunk_budget_us,
            if run.per_chunk.p99_us <= report.chunk_budget_us {
                "ok"
            } else {
                "MISSED"
            }
        );
    }
    println!(
        "one GMCP message: {:.2} ns/byte at 32 KB, {:.2} ns/byte at 1 MB",
        report.gmcp_32kb_ns_per_byte, report.gmcp_1mb_ns_per_byte
    );
}
