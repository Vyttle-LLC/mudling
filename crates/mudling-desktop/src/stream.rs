//! Feeds replayed lines into the chat at a steady, real pace.

use crate::chat::Chat;
use gpui::{App, Entity, Task};
use mudling_core::replay::ReplayFeed;
use std::time::{Duration, Instant};

/// Appends `lines_per_second` lines from `feed` until the task is dropped.
/// The count is kept against the clock, so timer jitter never changes the
/// pace, only how lines group into frames.
pub fn stream(
    chat: Entity<Chat>,
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
            chat.update(cx, |chat, cx| {
                for _ in sent..due {
                    chat.push_server_line(feed.next_line(), cx);
                }
            });
            sent = due;
        }
    })
}
