//! The lines a dog logs around a subscription that ended, on a clock the tests set.

use std::time::Duration;

use tokio::time::Instant;

use super::{Outage, QUIET};

const STREAMS: [&str; 2] = ["process events", "config changes"];

fn after(start: Instant, secs: f64) -> Instant {
    start + Duration::from_secs_f64(secs)
}

#[test]
fn a_failure_inside_the_quiet_window_is_not_logged_and_the_success_after_it_says_how_long() {
    for stream in STREAMS {
        let start = Instant::now();
        let mut outage = Outage::new(stream);
        assert_eq!(
            outage.ended(start),
            format!(
                "paddock: {stream} ended; the connection to the shepherd closed, as a `shep daemon reload` does; subscribing again"
            )
        );
        assert_eq!(outage.failed(after(start, 0.0), &"refused"), None);
        assert_eq!(
            outage.opened(after(start, 1.0)),
            Some(format!(
                "paddock: rejoined the shepherd after 1.0 s; {stream} resumed"
            ))
        );
        assert_eq!(outage.opened(after(start, 2.0)), None, "one line per end");
    }
}

#[test]
fn a_failure_past_the_quiet_window_is_logged_and_so_is_the_success_after_it() {
    for stream in STREAMS {
        let start = Instant::now();
        let mut outage = Outage::new(stream);
        outage.ended(start);
        assert_eq!(outage.failed(after(start, 4.9), &"refused"), None);
        assert_eq!(
            outage.failed(start + QUIET, &"refused"),
            Some(format!("paddock: subscribing to {stream} failed: refused")),
            "five seconds after the end is past the window"
        );
        assert!(outage.failed(after(start, 9.0), &"refused").is_some());
        assert_eq!(
            outage.opened(after(start, 12.0)),
            Some(format!(
                "paddock: rejoined the shepherd after 12.0 s; {stream} resumed"
            ))
        );
    }
}

#[test]
fn a_subscribe_at_start_up_logs_nothing_when_it_opens_and_its_failure_is_logged() {
    for stream in STREAMS {
        let mut outage = Outage::new(stream);
        assert_eq!(outage.opened(Instant::now()), None);
        let outage = Outage::new(stream);
        assert_eq!(
            outage.failed(Instant::now(), &"refused"),
            Some(format!("paddock: subscribing to {stream} failed: refused")),
            "no end to wait out, so a failure is logged as today"
        );
    }
}

#[test]
fn a_second_end_starts_a_new_window() {
    let start = Instant::now();
    let mut outage = Outage::new("process events");
    outage.ended(start);
    outage.opened(after(start, 1.0));
    outage.ended(after(start, 100.0));
    assert_eq!(outage.failed(after(start, 101.0), &"refused"), None);
}
