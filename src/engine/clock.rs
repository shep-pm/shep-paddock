//! The engine's moments, and the wall-clock times they stand for.

use std::time::Duration;

use tokio::time::Instant;

use crate::book::Moment;

// A saved time up to a year old, such as a restored lease's grant, maps to a real moment.
const BEFORE_START: Duration = Duration::from_secs(365 * 24 * 60 * 60);

/// Turns the engine's [`Moment`]s into wall-clock times and back
///
/// `Moment(0)` is one year before the engine started, so every moment the
/// engine reads is a year's worth of milliseconds or more.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Clock {
    start: Instant,
    start_wall: jiff::Timestamp,
}

impl Clock {
    /// A clock that starts now
    pub fn new() -> Clock {
        let now = jiff::Timestamp::now();
        // Moments are whole milliseconds, so the start is too, and the two ways round agree.
        let start_wall = jiff::Timestamp::from_millisecond(now.as_millisecond()).unwrap_or(now);
        Clock {
            start: Instant::now(),
            start_wall,
        }
    }

    /// A clock that starts now and reads `start_wall` as the wall-clock time now
    #[cfg(test)]
    pub fn started_at(start_wall: jiff::Timestamp) -> Clock {
        Clock {
            start: Instant::now(),
            start_wall,
        }
    }

    /// The moment it is now
    pub fn moment(&self) -> Moment {
        Moment(0).plus(BEFORE_START.saturating_add(self.start.elapsed()))
    }

    /// The wall-clock time of `moment`, held at the ends of jiff's range
    pub fn wall(&self, moment: Moment) -> jiff::Timestamp {
        let millis = i128::from(self.start_wall.as_millisecond()) + i128::from(moment.0)
            - millis_of(BEFORE_START);
        i64::try_from(millis)
            .ok()
            .and_then(|millis| jiff::Timestamp::from_millisecond(millis).ok())
            .unwrap_or(if millis < 0 {
                jiff::Timestamp::MIN
            } else {
                jiff::Timestamp::MAX
            })
    }

    /// The moment of `wall`, held at `Moment(0)` for anything older
    pub fn moment_of(&self, wall: jiff::Timestamp) -> Moment {
        let millis = i128::from(wall.as_millisecond())
            - i128::from(self.start_wall.as_millisecond())
            + millis_of(BEFORE_START);
        Moment(u64::try_from(millis.max(0)).unwrap_or(u64::MAX))
    }

    /// The tokio instant of `moment`, or `None` past the end of tokio's time
    pub fn instant(&self, moment: Moment) -> Option<Instant> {
        let since_start = Moment(0).plus(BEFORE_START);
        if moment < since_start {
            return Some(self.start);
        }
        self.start.checked_add(moment.since(since_start))
    }
}

fn millis_of(duration: Duration) -> i128 {
    i128::try_from(duration.as_millis()).unwrap_or(i128::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    const YEAR_MS: u64 = 365 * 24 * 60 * 60 * 1000;

    #[tokio::test(start_paused = true)]
    async fn the_engine_starts_a_year_after_moment_zero() {
        let clock = Clock::new();

        assert_eq!(clock.moment(), Moment(YEAR_MS));
        assert_eq!(clock.moment_of(clock.start_wall), Moment(YEAR_MS));
        assert_eq!(clock.wall(Moment(YEAR_MS)), clock.start_wall);
    }

    #[tokio::test(start_paused = true)]
    async fn moments_follow_the_tokio_clock() {
        let clock = Clock::new();
        tokio::time::advance(Duration::from_secs(90)).await;

        assert_eq!(clock.moment(), Moment(YEAR_MS + 90_000));
        assert_eq!(
            clock.wall(clock.moment()),
            clock.start_wall + jiff::SignedDuration::from_secs(90)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn wall_and_moment_of_round_trip() {
        let clock = Clock::new();
        for moment in [0, 1, YEAR_MS - 1, YEAR_MS, YEAR_MS + 3_600_000] {
            assert_eq!(clock.moment_of(clock.wall(Moment(moment))), Moment(moment));
        }
    }

    /// A restored lease granted a month before the restart keeps its real grant time.
    #[tokio::test(start_paused = true)]
    async fn a_time_within_the_year_before_start_is_a_real_moment() {
        let clock = Clock::new();
        let month_ago = clock.start_wall - jiff::SignedDuration::from_hours(30 * 24);

        assert_eq!(
            clock.moment_of(month_ago),
            Moment(YEAR_MS - 30 * 24 * 3_600_000)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_time_older_than_a_year_is_moment_zero() {
        let clock = Clock::new();
        let older = clock.start_wall - jiff::SignedDuration::from_hours(400 * 24);

        assert_eq!(clock.moment_of(older), Moment(0));
    }

    #[tokio::test(start_paused = true)]
    async fn a_moment_maps_to_the_tokio_instant_it_names() {
        let clock = Clock::new();

        assert_eq!(clock.instant(Moment(0)), Some(clock.start));
        assert_eq!(
            clock.instant(Moment(YEAR_MS + 5_000)),
            Some(clock.start + Duration::from_secs(5))
        );
    }
}
