//! Align timestamped samples onto intervals.
//!
//! Feeds arrive at whatever rate they like and the caller works on intervals
//! of its own choosing. This crate is the step between, and it is a pure
//! calculation: it reads no clock, opens no file, and takes the interval it
//! is aligning as an argument.
//!
//! # The rules
//!
//! A metric's [`Kind`] says how its samples become one number:
//!
//! * [`Kind::Power`] is the interval's **time-weighted mean**. Each sample is
//!   held until the next one and weighted by the span it held; the last
//!   sample opens no span and carries no weight. Nothing is back-filled
//!   before the first sample either.
//! * [`Kind::Energy`] is the **sum** of the values inside the interval. One
//!   energy sample covers the whole interval; zero energy samples leave the
//!   metric absent from the result rather than missing from it.
//! * [`Kind::Counter`] is the **rise** from the first sample to the last. A
//!   counter that falls is [`AlignError::CounterWentBackwards`], not an
//!   arithmetic guess.
//!
//! # Coverage is the sampled span
//!
//! A feed does not declare its rate, so "how much of this interval did we
//! see?" cannot be a fraction of an expected row count. For power and counter
//! metrics it is the span from the first sample to the last, over the
//! interval's length; fewer than two samples span nothing, so coverage is
//! zero. A metric whose coverage is below the configured minimum, or whose
//! coverage is zero, is [`Reading::Missing`] carrying the coverage it had and
//! **no value**.
//!
//! # Nothing is interpolated
//!
//! A gap is reported as a gap. The alternative is a plausible number nobody
//! can tell from a measured one.
//!
//! # What a duplicate is
//!
//! Two samples of one metric at one instant is a feed that replayed, not a
//! measurement that happened twice. The later one in caller order wins and
//! the discard is counted in the reading's `duplicates`.
//!
//! # Timestamps are plain seconds
//!
//! A [`Timestamp`] is seconds since the Unix epoch as `i64`. **This crate
//! does no time-zone work**: it never reads a wall clock and never converts
//! between zones. Whatever a local day means is the caller's business, and a
//! day whose clock moved still walks as whole intervals in UTC seconds.

#![deny(missing_docs)]

use std::collections::BTreeMap;

/// Seconds since the Unix epoch.
pub type Timestamp = i64;

/// How one metric's samples become one number for one interval.
///
/// The kind is carried on each [`Sample`] and is never inferred from a unit
/// string or from the shape of the data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// A power-like metric: the interval's time-weighted mean.
    Power,
    /// An energy row already attributed to an instant inside the interval:
    /// the sum.
    Energy,
    /// A monotone cumulative counter: the rise from the first sample to the
    /// last inside the interval.
    Counter,
}

/// One measurement of one metric at one instant.
///
/// Build one with [`Sample::new`], which refuses a non-finite value. The
/// fields are public, so a caller can also write a `Sample` literal directly;
/// in that case the finite-value check is the caller's to have done.
#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
    /// The metric this sample belongs to.
    pub metric: String,
    /// Seconds since the Unix epoch.
    pub at: Timestamp,
    /// The measured value.
    pub value: f64,
    /// How this sample aggregates.
    pub kind: Kind,
}

impl Sample {
    /// One sample. Refuses a non-finite value.
    ///
    /// ```
    /// use interval_align::{Kind, Sample};
    ///
    /// let sample = Sample::new("hall.kw", 1_772_949_600, 100.0, Kind::Power);
    /// assert!(sample.is_some());
    /// assert!(Sample::new("hall.kw", 1_772_949_600, f64::NAN, Kind::Power).is_none());
    /// ```
    #[must_use]
    pub fn new(metric: impl Into<String>, at: Timestamp, value: f64, kind: Kind) -> Option<Self> {
        if value.is_finite() {
            Some(Self {
                metric: metric.into(),
                at,
                value,
                kind,
            })
        } else {
            None
        }
    }
}

/// A positive length in seconds.
///
/// An interval has to have one, so the constructor refuses zero. The fields
/// of [`Interval`] are public, so a caller can also write the struct literal
/// directly; in that case the positive-length check is the caller's to have
/// done.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Duration {
    seconds: u64,
}

impl Duration {
    /// A positive length in seconds. Refuses zero.
    ///
    /// ```
    /// use interval_align::Duration;
    ///
    /// assert!(Duration::seconds(900).is_some());
    /// assert!(Duration::seconds(0).is_none());
    /// ```
    #[must_use]
    pub fn seconds(seconds: u64) -> Option<Self> {
        (seconds > 0).then_some(Self { seconds })
    }

    /// The length in seconds.
    #[must_use]
    pub const fn as_secs(&self) -> u64 {
        self.seconds
    }
}

/// A half-open window of time: `[start, start + length)`.
///
/// Any length is allowed; fifteen minutes is only this documentation's
/// example. Start and end are [`Timestamp`]s, seconds since the Unix epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Interval {
    /// The first second of the interval.
    pub start: Timestamp,
    /// How long the interval is.
    pub length: Duration,
}

impl Interval {
    /// The first second past the interval, saturating at `i64::MAX`.
    #[must_use]
    pub const fn end(&self) -> Timestamp {
        self.start.saturating_add(self.length.as_secs() as i64)
    }

    /// Whether `at` lies inside the interval: `start <= at < end`.
    #[must_use]
    pub const fn contains(&self, at: Timestamp) -> bool {
        at >= self.start && at < self.end()
    }
}

/// What alignment is allowed to accept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rules {
    /// The minimum coverage a metric needs before its number is used.
    pub min_coverage_pct: u8,
}

/// One metric's verdict for one interval.
///
/// [`Reading::Missing`] carries no value on purpose. There is no field a
/// caller could read "anyway", which is what keeps a thin interval out of a
/// downstream number.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Reading {
    /// The metric was measured well enough to use.
    Value {
        /// The aggregated value.
        value: f64,
        /// How much of the interval this number was measured over, 0-100.
        coverage_pct: u8,
        /// How many duplicate samples were discarded reaching it.
        duplicates: u32,
    },
    /// The metric was sampled too thinly. Nothing is filled in.
    Missing {
        /// How much of the interval was seen, 0-100.
        coverage_pct: u8,
    },
}

/// What alignment refuses.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum AlignError {
    /// A sample belongs to another interval. This is a bug in the caller,
    /// not a row worth dropping, so it is a typed error rather than a silent
    /// skip.
    #[error("sample of {metric} at {at} is not in the interval being aligned")]
    SampleOutsideInterval {
        /// The metric of the offending sample.
        metric: String,
        /// The offending sample's instant, seconds since the Unix epoch.
        at: Timestamp,
    },
    /// A counter fell between its first and last sample. A meter that rolled
    /// over or reset needs a person, not a guess.
    #[error("counter {metric} went backwards inside one interval")]
    CounterWentBackwards {
        /// The metric whose counter fell.
        metric: String,
    },
}

/// Samples of one interval, as one number per metric.
///
/// A metric with no sample at all is absent from the map rather than
/// `Missing`: nothing was expected of it here, and an expectation is the
/// caller's to hold. The map is a [`BTreeMap`], so the same input always
/// yields the same iteration order.
///
/// A metric's kind is taken from its last sample in caller order; a caller
/// that mixes kinds for one metric is a caller to fix.
///
/// # Errors
///
/// Returns [`AlignError::SampleOutsideInterval`] when a sample's timestamp
/// lies outside `[interval.start, interval.end)`, and
/// [`AlignError::CounterWentBackwards`] when a counter metric that passed the
/// coverage gate falls.
///
/// ```
/// use interval_align::{align, Duration, Interval, Kind, Reading, Rules, Sample};
///
/// // Synthetic example data.
/// let start = 1_772_949_600;
/// let interval = Interval { start, length: Duration::seconds(900).expect("positive") };
/// let samples = [
///     Sample::new("hall.kw", start, 100.0, Kind::Power).expect("finite"),
///     Sample::new("hall.kw", start + 300, 200.0, Kind::Power).expect("finite"),
///     Sample::new("hall.kw", start + 720, 100.0, Kind::Power).expect("finite"),
/// ];
/// let aligned = align(&samples, interval, &Rules { min_coverage_pct: 80 })
///     .expect("all samples inside the interval");
/// assert!(matches!(aligned["hall.kw"], Reading::Value { coverage_pct: 80, .. }));
/// ```
pub fn align(
    samples: &[Sample],
    interval: Interval,
    rules: &Rules,
) -> Result<BTreeMap<String, Reading>, AlignError> {
    for sample in samples {
        if !interval.contains(sample.at) {
            return Err(AlignError::SampleOutsideInterval {
                metric: sample.metric.clone(),
                at: sample.at,
            });
        }
    }

    // One point per (metric, instant), the later one in caller order winning.
    // A `BTreeMap` keyed by (metric, instant) keeps each metric's survivors
    // in time order for free, which is what the aggregations below need.
    let mut points: BTreeMap<(String, Timestamp), f64> = BTreeMap::new();
    let mut kind_by_metric: BTreeMap<String, Kind> = BTreeMap::new();
    let mut brought: BTreeMap<String, u32> = BTreeMap::new();
    for sample in samples {
        points.insert((sample.metric.clone(), sample.at), sample.value);
        kind_by_metric.insert(sample.metric.clone(), sample.kind);
        *brought.entry(sample.metric.clone()).or_default() += 1;
    }

    let mut series: BTreeMap<String, Vec<(Timestamp, f64)>> = BTreeMap::new();
    for ((metric, at), value) in points {
        series.entry(metric).or_default().push((at, value));
    }

    let mut readings = BTreeMap::new();
    for (metric, points) in series {
        let duplicates = brought[&metric] - points.len() as u32;
        let kind = kind_by_metric[&metric];
        let reading = reading_of(
            &metric,
            kind,
            &points,
            interval,
            rules.min_coverage_pct,
            duplicates,
        )?;
        readings.insert(metric, reading);
    }
    Ok(readings)
}

/// One metric's verdict: enough coverage and a number, or how little there
/// was.
///
/// The duplicates are already gone; `duplicates` says how many were. The
/// coverage gate comes before the aggregation, so a counter that falls is
/// only a typed error when its coverage passed the gate in the first place.
fn reading_of(
    metric: &str,
    kind: Kind,
    points: &[(Timestamp, f64)],
    interval: Interval,
    min_coverage_pct: u8,
    duplicates: u32,
) -> Result<Reading, AlignError> {
    let coverage_pct = coverage_pct_of(points, kind, interval);
    if coverage_pct == 0 || coverage_pct < min_coverage_pct {
        return Ok(Reading::Missing { coverage_pct });
    }
    let value = match kind {
        Kind::Energy => points.iter().map(|&(_, value)| value).sum(),
        Kind::Power => time_weighted_mean(points),
        Kind::Counter => {
            let first = points[0].1;
            let last = points[points.len() - 1].1;
            if last < first {
                return Err(AlignError::CounterWentBackwards {
                    metric: metric.to_owned(),
                });
            }
            last - first
        }
    };
    Ok(Reading::Value {
        value,
        coverage_pct,
        duplicates,
    })
}

/// How much of the interval these points measured, as a percent 0-100.
///
/// An energy feed is different in kind: a row stamped once inside the
/// interval *is* the interval's energy, and there is no gap between rows to
/// measure, so one sample covers the interval. A power or counter metric
/// measures a span, so it is the span from the first sample to the last over
/// the interval's length; fewer than two samples span nothing.
fn coverage_pct_of(points: &[(Timestamp, f64)], kind: Kind, interval: Interval) -> u8 {
    match kind {
        Kind::Energy => 100,
        Kind::Power | Kind::Counter => match (points.first(), points.last()) {
            (Some(&(first, _)), Some(&(last, _))) => {
                let span = (last - first) as u128;
                let length = interval.length.as_secs() as u128;
                (span * 100 / length) as u8
            }
            _ => 0,
        },
    }
}

/// Each sample weighted by the span until the next one.
///
/// The last sample opens no span and so carries no weight. Callers reach
/// this only when the span is non-zero, because zero coverage is `Missing`.
fn time_weighted_mean(points: &[(Timestamp, f64)]) -> f64 {
    let weighted: f64 = points
        .windows(2)
        .map(|pair| pair[0].1 * (pair[1].0 - pair[0].0) as f64)
        .sum();
    let span = (points[points.len() - 1].0 - points[0].0) as f64;
    weighted / span
}

#[cfg(test)]
mod tests {
    use super::*;

    // Synthetic instants; nothing here reads a clock.
    const START: Timestamp = 1_772_949_600;

    fn duration(secs: u64) -> Duration {
        Duration::seconds(secs).expect("a positive length")
    }

    fn sample(metric: &str, offset: i64, value: f64, kind: Kind) -> Sample {
        Sample::new(metric, START + offset, value, kind).expect("a finite value")
    }

    #[test]
    fn a_sample_refuses_a_non_finite_value() {
        assert!(Sample::new("hall.kw", START, f64::NAN, Kind::Power).is_none());
        assert!(Sample::new("hall.kw", START, f64::INFINITY, Kind::Power).is_none());
        assert!(sample("hall.kw", 0, 1.0, Kind::Power).value == 1.0);
    }

    #[test]
    fn a_duration_refuses_zero() {
        assert!(Duration::seconds(0).is_none());
        assert_eq!(duration(900).as_secs(), 900);
    }

    #[test]
    fn an_interval_contains_its_start_but_not_its_end() {
        let interval = Interval {
            start: START,
            length: duration(900),
        };
        assert!(interval.contains(START));
        assert!(interval.contains(START + 899));
        assert!(!interval.contains(START + 900));
        assert_eq!(interval.end(), START + 900);
    }

    #[test]
    fn an_interval_end_saturates() {
        let interval = Interval {
            start: i64::MAX - 10,
            length: duration(900),
        };
        assert_eq!(interval.end(), i64::MAX);
    }

    #[test]
    fn empty_input_aligns_to_an_empty_map() {
        let interval = Interval {
            start: START,
            length: duration(900),
        };
        let aligned = align(
            &[],
            interval,
            &Rules {
                min_coverage_pct: 80,
            },
        )
        .expect("empty aligns");
        assert!(aligned.is_empty());
    }
}
