//! A local day that gains or loses an hour is still whole quarter-hours.
//!
//! Intervals are UTC (`Interval15::month_of` says so in as many words), and a
//! tariff's time-of-use periods are site-local and belong to the tariff
//! (E1.1). The claim this test exists to prove is what falls out of that: when
//! a site's local day is 23 or 25 hours long, the UTC intervals covering it
//! have no gap, no duplicate and no partial quarter-hour — alignment never has
//! to know that a clock moved.
//!
//! The two boundaries are derived by hand rather than by a time-zone
//! database, because the approved dependency list carries none. America/
//! Chicago in 2026 keeps `CST = UTC-6` and `CDT = UTC-5`, and the changes fall
//! on 8 March and 1 November:
//!
//! * The spring day runs 2026-03-08T00:00 CST → 2026-03-09T00:00 CDT, which
//!   is 2026-03-08T06:00Z → 2026-03-09T05:00Z: 23 hours, 92 intervals.
//! * The autumn day runs 2026-11-01T00:00 CDT → 2026-11-02T00:00 CST, which
//!   is 2026-11-01T05:00Z → 2026-11-02T06:00Z: 25 hours, 100 intervals.

use std::collections::BTreeSet;

use eclairos_domain::{Interval15, UnixMinutes};

/// 2026-03-08T06:00Z, midnight CST on the day the clock springs forward.
///
/// 20 520 days from the epoch to 2026-03-08, plus the six hours CST sits
/// behind UTC: (20 520 × 1440) + 360 minutes.
const SPRING_DAY_START_MIN: i64 = 29_549_160;
/// 2026-03-09T05:00Z, midnight CDT the next day.
const SPRING_DAY_END_MIN: i64 = SPRING_DAY_START_MIN + 23 * 60;

/// 2026-11-01T05:00Z, midnight CDT on the day the clock falls back.
///
/// 20 758 days from the epoch to 2026-11-01, plus the five hours CDT sits
/// behind UTC: (20 758 × 1440) + 300 minutes.
const AUTUMN_DAY_START_MIN: i64 = 29_891_820;
/// 2026-11-02T06:00Z, midnight CST the next day.
const AUTUMN_DAY_END_MIN: i64 = AUTUMN_DAY_START_MIN + 25 * 60;

fn interval_at(minutes: i64) -> Interval15 {
    let start = UnixMinutes::new(minutes).expect("a 2026 instant is after the epoch");
    Interval15::new(start).expect("a local midnight in a whole-hour offset is a quarter hour")
}

/// Every interval from `start` up to but not including `end`.
fn intervals_between(start: i64, end: i64) -> Vec<Interval15> {
    let mut walked = Vec::new();
    let mut interval = interval_at(start);
    while interval.start().get() < end {
        walked.push(interval);
        interval = interval.next();
    }
    walked
}

/// The starts are strictly increasing, so no quarter-hour repeats and none is
/// skipped. This is the property a DST change would break if intervals were
/// local, and cannot break while they are UTC.
fn assert_no_gap_and_no_duplicate(intervals: &[Interval15]) {
    let starts: Vec<i64> = intervals
        .iter()
        .map(|interval| interval.start().get())
        .collect();
    let unique: BTreeSet<i64> = starts.iter().copied().collect();
    assert_eq!(unique.len(), starts.len(), "a quarter-hour repeated");
    for pair in starts.windows(2) {
        assert_eq!(
            pair[1] - pair[0],
            Interval15::LENGTH_MINUTES,
            "a gap between {} and {}",
            pair[0],
            pair[1]
        );
    }
}

#[test]
fn a_twenty_three_hour_local_day_is_ninety_two_whole_intervals() {
    let day = intervals_between(SPRING_DAY_START_MIN, SPRING_DAY_END_MIN);

    assert_eq!(day.len(), 92);
    assert_no_gap_and_no_duplicate(&day);
    assert_eq!(
        day.last().map(|interval| interval.next().start().get()),
        Some(SPRING_DAY_END_MIN),
        "the last interval ends exactly at the next local midnight"
    );
}

#[test]
fn a_twenty_five_hour_local_day_is_one_hundred_whole_intervals() {
    let day = intervals_between(AUTUMN_DAY_START_MIN, AUTUMN_DAY_END_MIN);

    assert_eq!(day.len(), 100);
    assert_no_gap_and_no_duplicate(&day);
    assert_eq!(
        day.last().map(|interval| interval.next().start().get()),
        Some(AUTUMN_DAY_END_MIN),
        "the last interval ends exactly at the next local midnight"
    );
}

/// The hand-derived instants are what the module doc says they are. Without
/// this, a typo in a constant would quietly move the whole day and both counts
/// would still look plausible.
#[test]
fn the_two_boundaries_are_the_utc_instants_the_doc_derives() {
    let spring = interval_at(SPRING_DAY_START_MIN);
    let autumn = interval_at(AUTUMN_DAY_START_MIN);

    // 2026-03-08T06:00Z and 2026-11-01T05:00Z as seconds since the epoch.
    assert_eq!(spring.start().get() * 60, 1_772_949_600);
    assert_eq!(autumn.start().get() * 60, 1_793_509_200);
    assert_eq!(spring.month_of().to_string(), "2026-03");
    assert_eq!(autumn.month_of().to_string(), "2026-11");
}

/// A UTC day is always ninety-six, whatever the site's clock did. The local
/// day is the thing that varies, which is the point.
#[test]
fn a_utc_day_is_ninety_six_intervals_on_both_change_days() {
    for start in [SPRING_DAY_START_MIN, AUTUMN_DAY_START_MIN] {
        let utc_day = intervals_between(start, start + 24 * 60);

        assert_eq!(utc_day.len(), 96);
        assert_no_gap_and_no_duplicate(&utc_day);
    }
}
