//! How long the game takes to answer, read from traffic that flows anyway:
//! nothing is ever sent just to time it.
//!
//! A command sent on a quiet connection starts the clock, and the next bytes
//! from the server stop it. A game's own output can land in that gap, and a
//! game that runs in ticks answers late by up to a tick, so what's shown is
//! the median of the last few answers, which one stray reading can't move. A
//! command sent while the server is still talking isn't timed: whatever
//! arrives next was on its way already.
//!
//! So the reading is the wait a player feels, tick included, not the network
//! alone. On a quiet connection nothing is timed and the last reading stands,
//! however old it gets, until the next command.

use crate::persist::Timestamp;
use std::collections::VecDeque;
use std::time::Duration;

/// Answers the median is taken over.
const ANSWERS_KEPT: usize = 9;
/// Silence from the server before a command is timed.
const QUIET_BEFORE_TIMING_MS: i64 = 200;
/// Longer than this and it was no answer: the command had none, and the
/// server spoke later of its own accord.
const LONGEST_ANSWER_MS: i64 = 10_000;

#[derive(Debug, Default)]
pub(super) struct Latency {
    /// When the command being timed went out.
    sent: Option<Timestamp>,
    last_read: Option<Timestamp>,
    answers: VecDeque<Duration>,
}

impl Latency {
    /// A command went out.
    pub(super) fn sent(&mut self, now: Timestamp) {
        let server_talking = self
            .last_read
            .is_some_and(|read| now.0 - read.0 < QUIET_BEFORE_TIMING_MS);
        if self.sent.is_none() && !server_talking {
            self.sent = Some(now);
        }
    }

    /// The server sent something. Returns the new reading, if this answered
    /// a timed command.
    pub(super) fn received(&mut self, now: Timestamp) -> Option<Duration> {
        self.last_read = Some(now);
        let waited = now.0 - self.sent.take()?.0;
        if !(0..=LONGEST_ANSWER_MS).contains(&waited) {
            return None;
        }
        if self.answers.len() == ANSWERS_KEPT {
            self.answers.pop_front();
        }
        self.answers.push_back(Duration::from_millis(waited as u64));
        let mut sorted: Vec<Duration> = self.answers.iter().copied().collect();
        sorted.sort();
        Some(sorted[sorted.len() / 2])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: i64 = 1;

    /// A command at `at`, answered `after` later, on a quiet connection.
    fn answer(latency: &mut Latency, at: i64, after: i64) -> Option<Duration> {
        latency.sent(Timestamp(at));
        latency.received(Timestamp(at + after))
    }

    #[test]
    fn a_command_is_timed_to_the_next_bytes_from_the_server() {
        let mut latency = Latency::default();
        assert_eq!(latency.received(Timestamp(0)), None, "nothing was sent");
        assert_eq!(
            answer(&mut latency, 1_000, 38 * MS),
            Some(Duration::from_millis(38))
        );
        assert_eq!(
            latency.received(Timestamp(1_100)),
            None,
            "one command, one answer"
        );
    }

    #[test]
    fn one_stray_reading_moves_nothing() {
        let mut latency = Latency::default();
        answer(&mut latency, 1_000, 40);
        answer(&mut latency, 2_000, 40);
        let stray = answer(&mut latency, 3_000, 1);
        assert_eq!(stray, Some(Duration::from_millis(40)));
    }

    #[test]
    fn a_command_sent_while_the_server_talks_is_not_timed() {
        let mut latency = Latency::default();
        latency.received(Timestamp(1_000));
        latency.sent(Timestamp(1_050));
        assert_eq!(latency.received(Timestamp(1_060)), None);
    }

    #[test]
    fn a_command_with_no_answer_gives_no_reading() {
        let mut latency = Latency::default();
        latency.sent(Timestamp(1_000));
        assert_eq!(latency.received(Timestamp(70_000)), None);
        assert_eq!(
            answer(&mut latency, 80_000, 30).map(|d| d.as_millis()),
            Some(30)
        );
    }

    #[test]
    fn a_clock_set_back_gives_no_reading() {
        let mut latency = Latency::default();
        latency.sent(Timestamp(1_000));
        assert_eq!(latency.received(Timestamp(500)), None);
    }
}
