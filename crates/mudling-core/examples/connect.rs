//! Connects to a server and prints what it says; lines typed on stdin are
//! sent.
//!
//! ```text
//! cargo run -p mudling-core --example connect -- <host>:<port>
//! ```

use mudling_core::net::{self, ConnectOptions, Event};
use mudling_core::telnet;
use std::io::{BufRead, Write};
use std::process::ExitCode;

fn main() -> ExitCode {
    let Some((host, port)) = std::env::args().nth(1).and_then(|address| {
        let (host, port) = address.rsplit_once(':')?;
        Some((host.to_owned(), port.parse().ok()?))
    }) else {
        eprintln!("usage: connect <host>:<port>");
        return ExitCode::from(2);
    };

    let runtime = tokio::runtime::Runtime::new().expect("a tokio runtime starts");
    let (connection, mut events) = net::connect(
        runtime.handle(),
        ConnectOptions {
            host,
            port,
            connect_timeout: net::DEFAULT_CONNECT_TIMEOUT,
        },
    );

    let typed = connection.clone();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            let mut encoded = Vec::new();
            telnet::encode_line(line.as_bytes(), &mut encoded);
            typed.write(encoded);
        }
        typed.close();
    });

    let mut parser = telnet::Parser::new();
    let mut parsed = Vec::new();
    while let Some(event) = events.blocking_recv() {
        let Event::Received(bytes) = event else {
            println!("\n[{event:?}]");
            continue;
        };
        if let Err(error) = parser.feed(bytes, &mut parsed) {
            println!("\n[{error}]");
        }
        connection.write(parser.take_output());
        for event in parsed.drain(..) {
            match event {
                telnet::Event::Data(text) => {
                    print!("{}", String::from_utf8_lossy(&text));
                    // Prompts often arrive without a newline.
                    let _ = std::io::stdout().flush();
                }
                event => println!("\n[{event:?}]"),
            }
        }
    }
    ExitCode::SUCCESS
}
