//! Feeds a recording through the session at a steady, real pace.
//!
//! [`mudling_core::session::Session::replay`] takes a whole recording and
//! runs it as one connection, so a paced replay is many short ones: each
//! tick's lines are a recording of their own.

use crate::shell::Shell;
use gpui::{App, Entity, Task};
use mudling_core::replay::ReplayFeed;
use mudling_core::telnet::Bytes;
use std::time::{Duration, Instant};

/// Lines in one read from the recording, about what a socket read holds.
const LINES_PER_READ: usize = 32;

/// `lines` as reads, as a socket would hand them over.
pub fn reads(lines: &[impl AsRef<str>]) -> Vec<Bytes> {
    lines
        .chunks(LINES_PER_READ)
        .map(|lines| {
            let mut read = String::new();
            for line in lines {
                read.push_str(line.as_ref());
                read.push('\n');
            }
            Bytes::from(read)
        })
        .collect()
}

pub fn next_reads(feed: &mut ReplayFeed, count: u64) -> Vec<Bytes> {
    let lines: Vec<String> = (0..count).map(|_| feed.next_line().to_owned()).collect();
    reads(&lines)
}

/// Replays `lines_per_second` lines from `feed` until the task is dropped.
/// The count is kept against the clock, so timer jitter never changes the
/// pace, only how lines group into frames.
pub fn stream(
    shell: Entity<Shell>,
    mut feed: ReplayFeed,
    lines_per_second: f64,
    cx: &mut App,
) -> Task<()> {
    let tick = Duration::from_secs_f64(1.0 / lines_per_second);
    cx.spawn(async move |cx| {
        let started = Instant::now();
        let mut sent = 0u64;
        loop {
            cx.background_executor().timer(tick).await;
            let due = (started.elapsed().as_secs_f64() * lines_per_second) as u64;
            if due > sent {
                let lines = next_reads(&mut feed, due - sent);
                shell.update(cx, |shell, _| shell.replay(lines));
            }
            sent = due;
        }
    })
}
