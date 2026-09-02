//! Policy for the resident watcher: what the device events mean and how
//! long to wait before trying again after a lost session.

use std::time::Duration;

/// First pause before retrying a session start after a loss.
pub const RECONNECT_INITIAL: Duration = Duration::from_millis(3000);
/// Longest pause between session start attempts.
pub const RECONNECT_MAX: Duration = Duration::from_millis(30_000);

/// A change in the display's presence, or a request to stop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The display's printer interface appeared at this path.
    Arrived(String),
    /// The display's printer interface at this path went away.
    Removed(String),
    /// The watcher should release the display and exit.
    Quit,
}

/// Grows the pause between session start attempts while the display stays
/// present but the session keeps failing.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Reconnect {
    attempt: u32,
}

impl Reconnect {
    pub fn new() -> Self {
        Self::default()
    }

    /// The pause before the next attempt: 3, 6, 12, 24, then 30 s.
    pub fn next_delay(&mut self) -> Duration {
        let delay = (RECONNECT_INITIAL * (1u32 << self.attempt.min(4))).min(RECONNECT_MAX);
        self.attempt = self.attempt.saturating_add(1);
        delay
    }

    /// Attempts made since the last reset.
    pub fn attempts(&self) -> u32 {
        self.attempt
    }

    /// Forgets the failures after a session has been established.
    pub fn reset(&mut self) {
        self.attempt = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconnect_delays_double_and_cap() {
        let mut reconnect = Reconnect::new();
        let delays: Vec<u64> = (0..7).map(|_| reconnect.next_delay().as_millis() as u64).collect();
        assert_eq!(delays, [3000, 6000, 12_000, 24_000, 30_000, 30_000, 30_000]);
        assert_eq!(reconnect.attempts(), 7);
        reconnect.reset();
        assert_eq!(reconnect.attempts(), 0);
        assert_eq!(reconnect.next_delay(), RECONNECT_INITIAL);
    }
}
