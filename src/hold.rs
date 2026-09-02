//! Keeps a started display session alive with periodic Pings.

use std::time::{Duration, Instant};

use crate::session::{KeepaliveOutcome, OptionalReply, Session, SessionError};
use crate::transport::Transport;

/// Time between keepalive Pings, measured from the last outbound write.
/// The panel blanks after 5 s of silence; this leaves half a second of
/// margin over the loop's own overhead.
pub const KEEPALIVE_INTERVAL: Duration = Duration::from_millis(4500);
/// Consecutive retryable Ping write failures tolerated before the session
/// is given up.
pub const KEEPALIVE_WRITE_RETRIES: u32 = 3;
/// Base pause before retrying a Ping write; multiplied by the attempt
/// number and capped at [`KEEPALIVE_RETRY_MAX_BACKOFF`].
pub const KEEPALIVE_RETRY_BACKOFF: Duration = Duration::from_millis(500);
/// Longest pause before a Ping retry.
pub const KEEPALIVE_RETRY_MAX_BACKOFF: Duration = Duration::from_millis(2000);
/// Longest single sleep inside the loop, so a stop request is noticed
/// promptly.
const WAIT_SLICE: Duration = Duration::from_millis(100);

/// Something the holding loop did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HoldEvent {
    /// A Ping transferred. `drained` is how many frames the session had
    /// swept up, unasked for, before this Ping went out.
    Pinged(OptionalReply, u64),
    /// A Ping write transferred nothing and will be retried after a pause.
    Retrying { attempt: u32, error: SessionError },
    /// A stop was requested and honoured.
    Stopped,
}

/// Pause before the `attempt`th retry of a Ping write.
pub fn retry_backoff(attempt: u32) -> Duration {
    (KEEPALIVE_RETRY_BACKOFF * attempt).min(KEEPALIVE_RETRY_MAX_BACKOFF)
}

/// Pings on `interval` until `stop` returns true or the session is lost.
///
/// `last_outbound` is when the previous write happened, so the first Ping
/// lands one interval after the session start rather than immediately. A
/// retryable write failure is retried up to [`KEEPALIVE_WRITE_RETRIES`]
/// times with growing pauses; beyond that, or on any fatal failure, the
/// session is closed and the error returned.
pub fn hold<T: Transport>(
    session: &mut Session<T>,
    interval: Duration,
    last_outbound: Instant,
    stop: &dyn Fn() -> bool,
    on_event: &mut dyn FnMut(HoldEvent),
) -> Result<(), SessionError> {
    let mut retries = 0u32;
    let mut next = last_outbound + interval;
    loop {
        loop {
            if stop() {
                on_event(HoldEvent::Stopped);
                return Ok(());
            }
            let now = session.now();
            if now >= next {
                break;
            }
            session.sleep((next - now).min(WAIT_SLICE));
        }

        match session.ping() {
            KeepaliveOutcome::Sent(reply) => {
                retries = 0;
                next = session.now() + interval;
                on_event(HoldEvent::Pinged(reply, session.drained_frames()));
            }
            KeepaliveOutcome::Retryable(error) => {
                retries += 1;
                if retries > KEEPALIVE_WRITE_RETRIES {
                    return Err(session.close(error));
                }
                on_event(HoldEvent::Retrying {
                    attempt: retries,
                    error,
                });
                session.sleep(retry_backoff(retries));
                next = session.now();
            }
            KeepaliveOutcome::Fatal(error) => return Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::encode;
    use crate::session::testing::FakeClock;
    use crate::transport::{MockTransport, WriteError};
    use crate::wire;
    use std::cell::Cell;

    fn holding_session(mock: MockTransport) -> (Session<MockTransport>, FakeClock) {
        let clock = FakeClock::new();
        (Session::with_clock(mock, Box::new(clock.clone())), clock)
    }

    fn run_until(
        session: &mut Session<MockTransport>,
        interval: Duration,
        max_pings: u32,
    ) -> (Result<(), SessionError>, Vec<HoldEvent>) {
        let pings = Cell::new(0u32);
        let mut events = Vec::new();
        let start = session.now();
        let result = hold(
            session,
            interval,
            start,
            &|| pings.get() >= max_pings,
            &mut |event| {
                if matches!(event, HoldEvent::Pinged(..)) {
                    pings.set(pings.get() + 1);
                }
                events.push(event);
            },
        );
        (result, events)
    }

    #[test]
    fn pings_once_per_interval_until_stopped() {
        let (mut session, clock) = holding_session(MockTransport::new());
        let (result, events) = run_until(&mut session, KEEPALIVE_INTERVAL, 3);
        assert_eq!(result, Ok(()));
        assert_eq!(
            events,
            vec![
                HoldEvent::Pinged(OptionalReply::None, 0),
                HoldEvent::Pinged(OptionalReply::None, 0),
                HoldEvent::Pinged(OptionalReply::None, 0),
                HoldEvent::Stopped,
            ]
        );
        assert_eq!(clock.elapsed(), KEEPALIVE_INTERVAL * 3);
        assert!(session.is_open());
        let mock = session.into_transport();
        assert_eq!(mock.writes.len(), 3);
        assert!(mock.writes.iter().all(|w| *w == encode(&wire::keepalive_ping()).unwrap()));
    }

    #[test]
    fn honours_a_custom_interval() {
        let (mut session, clock) = holding_session(MockTransport::new());
        let (result, _) = run_until(&mut session, Duration::from_millis(500), 4);
        assert_eq!(result, Ok(()));
        assert_eq!(clock.elapsed(), Duration::from_millis(2000));
    }

    #[test]
    fn stop_before_the_first_ping_sends_nothing() {
        let (mut session, _) = holding_session(MockTransport::new());
        let (result, events) = run_until(&mut session, KEEPALIVE_INTERVAL, 0);
        assert_eq!(result, Ok(()));
        assert_eq!(events, vec![HoldEvent::Stopped]);
        assert!(session.into_transport().writes.is_empty());
    }

    #[test]
    fn retries_a_zero_byte_write_with_growing_pauses() {
        let mut mock = MockTransport::new();
        mock.queue_write_result(Err(WriteError::TimedOut))
            .queue_write_result(Err(WriteError::TimedOut))
            .queue_write_result(Ok(()));
        let (mut session, clock) = holding_session(mock);
        let (result, events) = run_until(&mut session, KEEPALIVE_INTERVAL, 1);
        assert_eq!(result, Ok(()));
        assert_eq!(
            events,
            vec![
                HoldEvent::Retrying {
                    attempt: 1,
                    error: SessionError::Write(WriteError::TimedOut)
                },
                HoldEvent::Retrying {
                    attempt: 2,
                    error: SessionError::Write(WriteError::TimedOut)
                },
                HoldEvent::Pinged(OptionalReply::None, 0),
                HoldEvent::Stopped,
            ]
        );
        let sleeps = clock.sleeps_ms();
        assert!(sleeps.contains(&500));
        assert!(sleeps.contains(&1000));
        assert_eq!(session.into_transport().writes.len(), 3);
    }

    #[test]
    fn gives_up_after_the_retry_budget() {
        let mut mock = MockTransport::new();
        for _ in 0..4 {
            mock.queue_write_result(Err(WriteError::TimedOut));
        }
        let (mut session, clock) = holding_session(mock);
        let (result, events) = run_until(&mut session, KEEPALIVE_INTERVAL, 1);
        assert_eq!(result, Err(SessionError::Write(WriteError::TimedOut)));
        assert!(!session.is_open());
        assert_eq!(events.len(), 3);
        let sleeps = clock.sleeps_ms();
        assert!(sleeps.contains(&500));
        assert!(sleeps.contains(&1000));
        assert!(sleeps.contains(&1500));
        assert_eq!(session.into_transport().writes.len(), 4);
    }

    #[test]
    fn a_fatal_ping_ends_the_hold() {
        let mut mock = MockTransport::new();
        mock.queue_write_result(Err(WriteError::Partial { written: 2 }));
        let (mut session, _) = holding_session(mock);
        let (result, events) = run_until(&mut session, KEEPALIVE_INTERVAL, 1);
        assert_eq!(
            result,
            Err(SessionError::Write(WriteError::Partial { written: 2 }))
        );
        assert!(events.is_empty());
        assert!(!session.is_open());
    }

    #[test]
    fn backoff_schedule() {
        let millis: Vec<u64> = (1..=5).map(|n| retry_backoff(n).as_millis() as u64).collect();
        assert_eq!(millis, [500, 1000, 1500, 2000, 2000]);
    }
}
