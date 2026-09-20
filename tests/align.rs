//! Ported acceptance tests. All timestamps and values are synthetic.
//!
//! The two DST boundaries are derived by hand in UTC rather than by a
//! time-zone database, because this crate carries no time-zone dependency.

use std::collections::BTreeSet;

use interval_align::{align, AlignError, Duration, Interval, Kind, Reading, Rules, Sample};

/// 2026-03-08T06:00Z, a synthetic UTC instant, in seconds:
/// `29_549_160 * 60`.
const SPRING_DAY_START_SECS: i64 = 29_549_160 * 60;
/// 2026-11-01T05:00Z, a synthetic UTC instant, in seconds:
/// `29_891_820 * 60`.
const AUTUMN_DAY_START_SECS: i64 = 29_891_820 * 60;

fn interval_of(start: i64, length_secs: u64) -> Interval {
    Interval {
        start,
        length: Duration::seconds(length_secs).expect("a positive length"),
    }
}

fn sample(metric: &str, at: i64, value: f64, kind: Kind) -> Sample {
    Sample::new(metric, at, value, kind).expect("a finite value")
}

fn value_of(reading: &Reading) -> f64 {
    match reading {
        Reading::Value { value, .. } => *value,
        Reading::Missing { coverage_pct } => {
            panic!("expected a value, got Missing {{ coverage_pct: {coverage_pct} }}")
        }
    }
}

/// The issue's hand example: 100 held for five minutes, then 200 held for
/// seven, and a last sample that opens no span. The expected mean is
/// (100 x 300 + 200 x 420) / 720 = 158.333…, written out rather than
/// recomputed by the test.
#[test]
fn power_aligns_to_the_time_weighted_mean() {
    let interval = interval_of(SPRING_DAY_START_SECS, 900);
    let samples = [
        sample("hall.kw", SPRING_DAY_START_SECS, 100.0, Kind::Power),
        sample("hall.kw", SPRING_DAY_START_SECS + 300, 200.0, Kind::Power),
        sample("hall.kw", SPRING_DAY_START_SECS + 720, 100.0, Kind::Power),
    ];

    let aligned = align(
        &samples,
        interval,
        &Rules {
            min_coverage_pct: 80,
        },
    )
    .expect("three samples align");

    match &aligned["hall.kw"] {
        Reading::Value {
            value,
            coverage_pct,
            duplicates,
        } => {
            assert!(
                (value - 158.333_333_333_333_33).abs() < 1e-9,
                "mean was {value}"
            );
            assert_eq!(*coverage_pct, 80);
            assert_eq!(*duplicates, 0);
        }
        Reading::Missing { coverage_pct } => {
            panic!("expected a value, got Missing {{ coverage_pct: {coverage_pct} }}")
        }
    }
    assert_eq!(aligned.len(), 1, "a metric with no sample is absent");
}

/// Six minutes of nine hundred seconds is 40 %, under the minimum. The
/// reading carries the coverage it had and no value at all.
#[test]
fn a_metric_under_the_minimum_is_missing_and_never_filled() {
    let interval = interval_of(SPRING_DAY_START_SECS, 900);
    let samples = [
        sample("hall.kw", SPRING_DAY_START_SECS, 100.0, Kind::Power),
        sample("hall.kw", SPRING_DAY_START_SECS + 180, 110.0, Kind::Power),
        sample("hall.kw", SPRING_DAY_START_SECS + 360, 120.0, Kind::Power),
    ];

    let aligned = align(
        &samples,
        interval,
        &Rules {
            min_coverage_pct: 80,
        },
    )
    .expect("three samples align");

    assert_eq!(aligned["hall.kw"], Reading::Missing { coverage_pct: 40 });
}

/// A replayed instant keeps the later value and is counted.
#[test]
fn a_duplicate_instant_keeps_the_later_value_and_is_counted() {
    let interval = interval_of(SPRING_DAY_START_SECS, 900);
    let samples = [
        sample("hall.kw", SPRING_DAY_START_SECS, 100.0, Kind::Power),
        sample("hall.kw", SPRING_DAY_START_SECS + 300, 999.0, Kind::Power),
        sample("hall.kw", SPRING_DAY_START_SECS + 300, 200.0, Kind::Power),
        sample("hall.kw", SPRING_DAY_START_SECS + 720, 100.0, Kind::Power),
    ];

    let aligned = align(
        &samples,
        interval,
        &Rules {
            min_coverage_pct: 80,
        },
    )
    .expect("four samples align");

    match &aligned["hall.kw"] {
        Reading::Value {
            value,
            coverage_pct,
            duplicates,
        } => {
            assert!(
                (value - 158.333_333_333_333_33).abs() < 1e-9,
                "the replayed 999 survived: {value}"
            );
            assert_eq!(*coverage_pct, 80);
            assert_eq!(*duplicates, 1);
        }
        Reading::Missing { coverage_pct } => {
            panic!("expected a value, got Missing {{ coverage_pct: {coverage_pct} }}")
        }
    }
}

/// Intervals are UTC, and a site's local day is the caller's business. The
/// two boundaries are derived by hand rather than by a time-zone database:
/// a 23-hour local day is 92 whole intervals and a 25-hour local day is 100,
/// both with no gap and no duplicate.
fn walk(start: i64, day_length_secs: i64) -> Vec<i64> {
    let mut starts = Vec::new();
    let mut next = start;
    let end = start + day_length_secs;
    while next < end {
        starts.push(next);
        next += 900;
    }
    starts
}

fn assert_no_gap_and_no_duplicate(starts: &[i64]) {
    let unique: BTreeSet<i64> = starts.iter().copied().collect();
    assert_eq!(unique.len(), starts.len(), "an interval repeated");
    for pair in starts.windows(2) {
        assert_eq!(pair[1] - pair[0], 900, "a gap between {}", pair[0]);
    }
    assert!(
        starts.windows(2).all(|pair| pair[1] > pair[0]),
        "starts are strictly increasing"
    );
}

#[test]
fn a_twenty_three_hour_local_day_is_ninety_two_whole_intervals() {
    assert_eq!(SPRING_DAY_START_SECS, 1_772_949_600);
    let starts = walk(SPRING_DAY_START_SECS, 23 * 3600);

    assert_eq!(starts.len(), 92);
    assert_no_gap_and_no_duplicate(&starts);
    assert_eq!(
        starts.last().copied(),
        Some(SPRING_DAY_START_SECS + 92 * 900 - 900),
        "the last interval ends exactly at the next local midnight"
    );
}

#[test]
fn a_twenty_five_hour_local_day_is_one_hundred_whole_intervals() {
    assert_eq!(AUTUMN_DAY_START_SECS, 1_793_509_200);
    let starts = walk(AUTUMN_DAY_START_SECS, 25 * 3600);

    assert_eq!(starts.len(), 100);
    assert_no_gap_and_no_duplicate(&starts);
    assert_eq!(
        starts.last().copied(),
        Some(AUTUMN_DAY_START_SECS + 100 * 900 - 900),
        "the last interval ends exactly at the next local midnight"
    );
}

/// One sample spans nothing, so there is no mean to take, whatever the
/// configured minimum says.
#[test]
fn one_sample_covers_nothing_even_when_nothing_is_required() {
    let interval = interval_of(SPRING_DAY_START_SECS, 900);
    let samples = [sample(
        "hall.kw",
        SPRING_DAY_START_SECS + 240,
        100.0,
        Kind::Power,
    )];

    let aligned = align(
        &samples,
        interval,
        &Rules {
            min_coverage_pct: 0,
        },
    )
    .expect("one sample aligns");

    assert_eq!(aligned["hall.kw"], Reading::Missing { coverage_pct: 0 });
}

/// A row stamped once inside the interval *is* the interval's energy, so one
/// sample covers it and sums to itself.
#[test]
fn one_energy_sample_covers_the_interval_and_sums_to_itself() {
    let interval = interval_of(SPRING_DAY_START_SECS, 900);
    let samples = [sample(
        "meter.kwh",
        SPRING_DAY_START_SECS + 540,
        42.0,
        Kind::Energy,
    )];

    let aligned = align(
        &samples,
        interval,
        &Rules {
            min_coverage_pct: 80,
        },
    )
    .expect("one row aligns");

    match &aligned["meter.kwh"] {
        Reading::Value {
            value,
            coverage_pct,
            duplicates,
        } => {
            assert!((value - 42.0).abs() < f64::EPSILON);
            assert_eq!(*coverage_pct, 100);
            assert_eq!(*duplicates, 0);
        }
        Reading::Missing { coverage_pct } => {
            panic!("expected a value, got Missing {{ coverage_pct: {coverage_pct} }}")
        }
    }
}

/// A counter aligns to its rise inside the interval.
#[test]
fn a_counter_aligns_to_its_rise() {
    let interval = interval_of(SPRING_DAY_START_SECS, 900);
    let samples = [
        sample(
            "meter.total",
            SPRING_DAY_START_SECS,
            1_000_000.0,
            Kind::Counter,
        ),
        sample(
            "meter.total",
            SPRING_DAY_START_SECS + 420,
            1_000_364.0,
            Kind::Counter,
        ),
    ];

    let aligned = align(
        &samples,
        interval,
        &Rules {
            min_coverage_pct: 0,
        },
    )
    .expect("a counter aligns");

    assert!((value_of(&aligned["meter.total"]) - 364.0).abs() < f64::EPSILON);
}

/// A counter that falls is a typed error, not a negative quietly returned.
#[test]
fn a_counter_that_falls_is_refused() {
    let interval = interval_of(SPRING_DAY_START_SECS, 900);
    let samples = [
        sample(
            "meter.total",
            SPRING_DAY_START_SECS,
            1_000_000.0,
            Kind::Counter,
        ),
        sample(
            "meter.total",
            SPRING_DAY_START_SECS + 540,
            7.0,
            Kind::Counter,
        ),
    ];

    let refused = align(
        &samples,
        interval,
        &Rules {
            min_coverage_pct: 0,
        },
    );

    assert_eq!(
        refused,
        Err(AlignError::CounterWentBackwards {
            metric: "meter.total".to_owned()
        })
    );
}

/// A sample stamped at the next interval's start is refused, not dropped.
#[test]
fn a_sample_from_another_interval_is_refused() {
    let interval = interval_of(SPRING_DAY_START_SECS, 900);
    let samples = [sample(
        "hall.kw",
        SPRING_DAY_START_SECS + 900,
        100.0,
        Kind::Power,
    )];

    let refused = align(
        &samples,
        interval,
        &Rules {
            min_coverage_pct: 80,
        },
    );

    assert_eq!(
        refused,
        Err(AlignError::SampleOutsideInterval {
            metric: "hall.kw".to_owned(),
            at: SPRING_DAY_START_SECS + 900,
        })
    );
}
