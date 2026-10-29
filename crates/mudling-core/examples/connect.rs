//! Connects to a server and prints its events; lines typed on stdin are sent.
//!
//! ```text
//! cargo run -p mudling-core --example connect -- <host>:<port>
//! ```

use mudling_core::net::{self, ConnectOptions};
use mudling_core::telnet::Event;
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
            client_name: "MUDling".to_owned(),
            client_version: env!("CARGO_PKG_VERSION").to_owned(),
            gmcp_supports: vec![
                "Core 1".to_owned(),
                "Char 1".to_owned(),
                "Room 1".to_owned(),
            ],
            login: None,
        },
    );

    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            connection.send_line(line);
        }
        connection.close();
    });

    while let Some(event) = events.blocking_recv() {
        match event {
            Event::Data(text) => {
                print!("{}", String::from_utf8_lossy(&text));
                // Prompts often arrive without a newline.
                let _ = std::io::stdout().flush();
            }
            event => println!("\n[{event:?}]"),
        }
    }
    ExitCode::SUCCESS
}
