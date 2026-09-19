//! Samples into one quarter-hour's readings. The one rule, in one place.
//!
//! Feeds arrive at whatever rate they like — a BMC polled every sixty
//! seconds, a collector's sub-minute gauges, a meter row every fifteen
//! minutes — and the tariff and the loops work on [`Interval15`]. This module
//! is the step between, and it is a calculation: it reads no clock, opens
//! nothing, and takes the interval it is aligning as an argument.
//!
//! **Coverage is the sampled span, not a count of samples.** A feed does not
//! declare its rate, so "how much of this quarter-hour did we see?" cannot be
//! a fraction of an expected row count. It is the span from the first sample
//! to the last over the interval's fifteen minutes: samples for the first six
//! minutes cover 40 %, and samples at minutes 0, 5 and 12 cover 80 %.
//!
//! **Nothing is filled and nothing is interpolated.** A metric under the
//! configured minimum is [`MetricReading::Missing`] carrying the coverage it
//! did have. A gap is reported as a gap; the alternative is a plausible
//! number nobody can tell from a measured one.
//!
//! **How a metric aggregates is declared, never inferred.** A `kWh` sample
//! does not say whether it is this interval's energy or a meter's running
//! total — OpenTelemetry's gauge-versus-sum distinction is a wire concept and
//! stops at the adapter. So [`AlignRules`] names the metrics that are monotone
//! counters and everything else takes the default for its unit. Guessing from
//! monotonicity would silently mis-bill the first interval of a meter that
//! happened to rise.

use std::collections::{BTreeMap, BTreeSet};

use crate::interval::{Interval15, UnixSeconds};
use crate::sample::{MetricName, Sample, Unit};
use crate::units::Percent;

/// Seconds in a minute. The fifteen comes from [`Interval15::length`]; this is
/// the only other book-keeping constant the span arithmetic needs, and
/// `an_interval_is_nine_hundred_seconds` is the seam that fails if either
/// spelling moves.
const SECONDS_PER_MINUTE: f64 = 60.0;

/// How one metric's samples become one number for one interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Aggregation {
    /// Power held between samples: the interval's time-weighted mean.
    TimeWeightedMean,
    /// Energy already attributed to an instant inside the interval: the sum.
    Sum,
    /// A monotone cumulative counter: the rise from the first sample to the
    /// last. See [`IntervalReading`] for what that is and is not.
    CounterDelta,
}

/// What alignment is allowed to accept, as a value the configuration supplies.
///
/// Private fields: the counter set and the minimum are invariants of one
/// decision, and [`AlignRules::aggregation_for`] is the only way to ask it.
#[derive(Debug, Clone, PartialEq)]
pub struct AlignRules {
    min_coverage: Percent,
    counters: BTreeSet<MetricName>,
}

/// One metric's number for one interval, with how much of the interval it was
/// measured over and how many duplicates were discarded reaching it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Reading {
    value: f64,
    unit: Unit,
    coverage: Percent,
    dupes: u32,
}

/// A metric that was measured well enough to use, or one that was not.
///
/// [`MetricReading::Missing`] carries no value on purpose. There is no field
/// a caller could read "anyway", which is what keeps a thin interval out of a
/// ledger.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MetricReading {
    Present(Reading),
    Missing { coverage: Percent },
}

/// Everything one quarter-hour was able to say about itself.
///
/// A metric with no sample at all is absent from the map rather than
/// `Missing`: nothing was expected of it here, and an expectation is
/// configuration's to hold (E4.3), not this calculation's to invent.
///
/// A [`Aggregation::CounterDelta`] reading is the rise between the first and
/// last sample **inside** this interval. The quarter-hour's true energy needs
/// the next interval's first reading, which the alignment of one interval does
/// not have and must not reach for; closing that boundary belongs to the fold
/// thread (E3.5).
#[derive(Debug, Clone, PartialEq)]
pub struct IntervalReading {
    interval: Interval15,
    metrics: BTreeMap<MetricName, MetricReading>,
}

/// What alignment refuses.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum AlignError {
    #[error("sample of {metric} at {at} is not in the interval being aligned")]
    SampleOutsideInterval { metric: MetricName, at: i64 },
    #[error("metric {metric} changed unit inside one interval")]
    UnitChanged { metric: MetricName },
    #[error("counter {metric} went backwards inside one interval")]
    CounterWentBackwards { metric: MetricName },
}

/// One sample, reduced to what alignment needs of it.
#[derive(Debug, Clone, Copy)]
struct Point {
    at: UnixSeconds,
    value: f64,
}

/// One metric's samples for one interval, in the unit they all share.
#[derive(Debug)]
struct Series {
    unit: Unit,
    points: Vec<Point>,
}

impl Aggregation {
    /// The default for a unit: power is a mean, energy is a sum.
    ///
    /// Only [`AlignRules::aggregation_for`] calls this. A second caller would
    /// be a second place that decides how a metric aggregates.
    const fn for_unit(unit: Unit) -> Self {
        match unit {
            Unit::Watts | Unit::Kilowatts => Self::TimeWeightedMean,
            Unit::KilowattHours => Self::Sum,
        }
    }
}

impl AlignRules {
    /// The minimum coverage a metric needs before its number is used.
    ///
    /// **ESTIMATE.** Eighty percent is a starting value, not a measured one:
    /// it is three missed minutes out of fifteen. E4.3's config file carries
    /// the site's own, and this constant is what it defaults to.
    pub const DEFAULT_MIN_COVERAGE_PCT: f64 = 80.0;

    /// Rules with the metrics that are monotone counters named.
    #[must_use]
    pub fn new(min_coverage: Percent, counters: impl IntoIterator<Item = MetricName>) -> Self {
        Self {
            min_coverage,
            counters: counters.into_iter().collect(),
        }
    }

    /// The ESTIMATE default minimum with no counters declared.
    #[must_use]
    pub fn default_rules() -> Self {
        let minimum = Percent::new(Self::DEFAULT_MIN_COVERAGE_PCT)
            .unwrap_or_else(|_| unreachable!("the default minimum is within [0, 100]"));
        Self::new(minimum, Vec::new())
    }

    #[must_use]
    pub const fn min_coverage(&self) -> Percent {
        self.min_coverage
    }

    /// How this metric aggregates. The one seam that decides.
    #[must_use]
    pub fn aggregation_for(&self, metric: &MetricName, unit: Unit) -> Aggregation {
        if self.counters.contains(metric) {
            Aggregation::CounterDelta
        } else {
            Aggregation::for_unit(unit)
        }
    }
}

impl Reading {
    #[must_use]
    pub const fn value(&self) -> f64 {
        self.value
    }

    #[must_use]
    pub const fn unit(&self) -> Unit {
        self.unit
    }

    /// How much of the interval this number was measured over.
    #[must_use]
    pub const fn coverage(&self) -> Percent {
        self.coverage
    }

    /// Samples discarded because a later one carried the same instant.
    #[must_use]
    pub const fn dupes(&self) -> u32 {
        self.dupes
    }
}

impl MetricReading {
    /// How much of the interval was measured, present or not.
    #[must_use]
    pub const fn coverage(&self) -> Percent {
        match self {
            Self::Present(reading) => reading.coverage,
            Self::Missing { coverage } => *coverage,
        }
    }
}

impl IntervalReading {
    #[must_use]
    pub const fn interval(&self) -> Interval15 {
        self.interval
    }

    /// What this interval said about one metric, if it said anything.
    #[must_use]
    pub fn get(&self, metric: &MetricName) -> Option<&MetricReading> {
        self.metrics.get(metric)
    }

    /// Every metric that was measured well enough to use, in name order.
    pub fn present(&self) -> impl Iterator<Item = (&MetricName, &Reading)> {
        self.metrics
            .iter()
            .filter_map(|(metric, reading)| match reading {
                MetricReading::Present(present) => Some((metric, present)),
                MetricReading::Missing { .. } => None,
            })
    }

    /// Every metric that was sampled too thinly, with the coverage it had.
    pub fn missing(&self) -> impl Iterator<Item = (&MetricName, Percent)> {
        self.metrics
            .iter()
            .filter_map(|(metric, reading)| match reading {
                MetricReading::Missing { coverage } => Some((metric, *coverage)),
                MetricReading::Present(_) => None,
            })
    }

    /// Samples this interval discarded as duplicates, across every metric.
    #[must_use]
    pub fn dupes(&self) -> u32 {
        self.present().map(|(_, reading)| reading.dupes).sum()
    }

    /// How many metrics this interval holds a verdict on.
    #[must_use]
    pub fn len(&self) -> usize {
        self.metrics.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.metrics.is_empty()
    }
}

/// One interval's samples, as one number per metric.
///
/// # Errors
///
/// Returns [`AlignError::SampleOutsideInterval`] when a sample belongs to
/// another quarter-hour — [`Interval15::containing`] is the one conversion and
/// the caller has already used it, so this is a bug in the caller rather than
/// a row worth dropping — [`AlignError::UnitChanged`] when one metric arrives
/// in two units, and [`AlignError::CounterWentBackwards`] when a declared
/// counter falls.
pub fn align(
    samples: &[Sample],
    interval: Interval15,
    rules: &AlignRules,
) -> Result<IntervalReading, AlignError> {
    let metrics = series_by_metric(samples, interval)?
        .into_iter()
        .map(|(metric, series)| {
            let aggregation = rules.aggregation_for(&metric, series.unit);
            reading_of(&metric, &series, aggregation, rules.min_coverage())
                .map(|reading| (metric, reading))
        })
        .collect::<Result<BTreeMap<_, _>, AlignError>>()?;
    Ok(IntervalReading { interval, metrics })
}

/// The samples of this interval, gathered per metric in the order they came.
fn series_by_metric(
    samples: &[Sample],
    interval: Interval15,
) -> Result<BTreeMap<MetricName, Series>, AlignError> {
    let mut series: BTreeMap<MetricName, Series> = BTreeMap::new();
    for sample in samples {
        if Interval15::containing(sample.at()) != interval {
            return Err(AlignError::SampleOutsideInterval {
                metric: sample.metric().clone(),
                at: sample.at().get(),
            });
        }
        let point = Point {
            at: sample.at(),
            value: sample.value(),
        };
        match series.get_mut(sample.metric()) {
            Some(existing) if existing.unit != sample.unit() => {
                return Err(AlignError::UnitChanged {
                    metric: sample.metric().clone(),
                })
            }
            Some(existing) => existing.points.push(point),
            None => {
                series.insert(
                    sample.metric().clone(),
                    Series {
                        unit: sample.unit(),
                        points: vec![point],
                    },
                );
            }
        }
    }
    Ok(series)
}

/// One metric's verdict: enough coverage and a number, or how little there was.
fn reading_of(
    metric: &MetricName,
    series: &Series,
    aggregation: Aggregation,
    min_coverage: Percent,
) -> Result<MetricReading, AlignError> {
    let (points, dupes) = deduped(&series.points);
    let coverage = coverage_of(&points, aggregation);
    if coverage == Percent::zero() || coverage < min_coverage {
        return Ok(MetricReading::Missing { coverage });
    }
    let value = aggregate(metric, &points, aggregation)?;
    Ok(MetricReading::Present(Reading {
        value,
        unit: series.unit,
        coverage,
        dupes,
    }))
}

/// One point per instant, the later one winning, and how many were discarded.
///
/// Two samples of one metric at one instant is a feed that replayed, not a
/// measurement that happened twice. The sort is stable, so "later" is the
/// feed's own order for samples that share an instant.
fn deduped(points: &[Point]) -> (Vec<Point>, u32) {
    let mut ordered = points.to_vec();
    ordered.sort_by_key(|point| point.at);
    let mut kept: Vec<Point> = Vec::with_capacity(ordered.len());
    let mut dupes = 0;
    for point in ordered {
        match kept.last_mut() {
            Some(previous) if previous.at == point.at => {
                *previous = point;
                dupes += 1;
            }
            _ => kept.push(point),
        }
    }
    (kept, dupes)
}

/// How much of the interval these points measured.
///
/// A mean and a counter delta both measure a gap between samples, so both are
/// the sampled span over the interval. A summed feed is different in kind: a
/// 15-minute meter row *is* the interval's energy, stamped once inside it, and
/// there is no gap between rows to measure — so it is fully covered by one row
/// and not covered at all by none.
/// Both refusals [`Percent`] can make are unreachable from here, and the
/// `unreachable!` says which: a sampled span is a difference between two
/// non-negative instants over a positive length, so the ratio is finite and
/// not negative, and a hundred is inside \[0, 100\]. Returning a `Result`
/// instead would put a variant on [`AlignError`] that no input could produce.
fn coverage_of(points: &[Point], aggregation: Aggregation) -> Percent {
    match aggregation {
        Aggregation::Sum if points.is_empty() => Percent::zero(),
        Aggregation::Sum => Percent::new(100.0)
            .unwrap_or_else(|_| unreachable!("a hundred percent is within [0, 100]")),
        Aggregation::TimeWeightedMean | Aggregation::CounterDelta => {
            Percent::of_ratio(sampled_span_seconds(points) / interval_seconds())
                .unwrap_or_else(|_| unreachable!("a sampled span over a quarter hour is finite"))
        }
    }
}

/// The interval's length in seconds, from the one spelling of the length.
fn interval_seconds() -> f64 {
    Interval15::length().get() * SECONDS_PER_MINUTE
}

/// First sample to last. Fewer than two samples span nothing.
fn sampled_span_seconds(points: &[Point]) -> f64 {
    match (points.first(), points.last()) {
        #[allow(clippy::cast_precision_loss)]
        (Some(first), Some(last)) => (last.at.get() - first.at.get()) as f64,
        _ => 0.0,
    }
}

/// These points as one number, by the rule that was declared for them.
fn aggregate(
    metric: &MetricName,
    points: &[Point],
    aggregation: Aggregation,
) -> Result<f64, AlignError> {
    match aggregation {
        Aggregation::TimeWeightedMean => Ok(time_weighted_mean(points)),
        Aggregation::Sum => Ok(points.iter().map(|point| point.value).sum()),
        Aggregation::CounterDelta => counter_delta(metric, points),
    }
}

/// Each sample weighted by the span until the next one.
///
/// The last sample opens no span and so carries no weight: nothing is held
/// past it and nothing is back-filled before the first. Callers reach this
/// only when the span is non-zero, because zero coverage is `Missing`.
fn time_weighted_mean(points: &[Point]) -> f64 {
    let weighted: f64 = points
        .windows(2)
        .map(|pair| {
            #[allow(clippy::cast_precision_loss)]
            let span = (pair[1].at.get() - pair[0].at.get()) as f64;
            pair[0].value * span
        })
        .sum();
    weighted / sampled_span_seconds(points)
}

/// The rise from the first sample to the last.
fn counter_delta(metric: &MetricName, points: &[Point]) -> Result<f64, AlignError> {
    match (points.first(), points.last()) {
        (Some(first), Some(last)) if last.value >= first.value => Ok(last.value - first.value),
        (Some(_), Some(_)) => Err(AlignError::CounterWentBackwards {
            metric: metric.clone(),
        }),
        _ => Ok(0.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interval::UnixMinutes;

    /// 2026-09-01T00:00Z, the fixture's first interval, in minutes.
    const FIXTURE_EPOCH_MIN: i64 = 29_803_680;

    fn interval() -> Interval15 {
        Interval15::new(UnixMinutes::new(FIXTURE_EPOCH_MIN).expect("after the epoch"))
            .expect("the fixture epoch is a quarter hour")
    }

    fn metric(name: &str) -> MetricName {
        MetricName::new(name).expect("a test metric name is a name")
    }

    /// A sample of `name` at `minute` minutes into the interval under test.
    fn at_minute(name: &str, minute: i64, value: f64, unit: Unit) -> Sample {
        let at = UnixSeconds::new((FIXTURE_EPOCH_MIN + minute) * 60).expect("after the epoch");
        Sample::new(at, metric(name), BTreeMap::new(), value, unit).expect("a finite sample")
    }

    fn rules() -> AlignRules {
        AlignRules::default_rules()
    }

    fn present(reading: &IntervalReading, name: &str) -> Reading {
        match reading.get(&metric(name)) {
            Some(MetricReading::Present(present)) => *present,
            other => panic!("expected {name} to be present, got {other:?}"),
        }
    }

    #[test]
    fn an_interval_is_nine_hundred_seconds() {
        assert!((interval_seconds() - 900.0).abs() < f64::EPSILON);
    }

    /// The issue's hand example: 100 kW held for five minutes, then 200 kW
    /// held for seven, and a last sample that opens no span. The expected
    /// mean is (100 × 300 + 200 × 420) / 720 = 158.3333… kW, written out
    /// rather than recomputed by the test.
    #[test]
    fn power_aligns_to_the_time_weighted_mean() {
        let samples = [
            at_minute("hall.kw", 0, 100.0, Unit::Kilowatts),
            at_minute("hall.kw", 5, 200.0, Unit::Kilowatts),
            at_minute("hall.kw", 12, 100.0, Unit::Kilowatts),
        ];

        let aligned = align(&samples, interval(), &rules()).expect("three samples align");

        let hall = present(&aligned, "hall.kw");
        assert!(
            (hall.value() - 158.333_333_333_333_33).abs() < 1e-9,
            "mean was {}",
            hall.value()
        );
        assert_eq!(hall.unit(), Unit::Kilowatts);
    }

    /// The same example is the boundary case for the default minimum: twelve
    /// minutes of fifteen is exactly 80 %.
    #[test]
    fn the_sampled_span_is_the_coverage() {
        let samples = [
            at_minute("hall.kw", 0, 100.0, Unit::Kilowatts),
            at_minute("hall.kw", 5, 200.0, Unit::Kilowatts),
            at_minute("hall.kw", 12, 100.0, Unit::Kilowatts),
        ];

        let aligned = align(&samples, interval(), &rules()).expect("three samples align");

        assert_eq!(
            present(&aligned, "hall.kw").coverage(),
            Percent::new(80.0).expect("a percentage")
        );
    }

    /// Six minutes of fifteen is 40 %, under the minimum. The reading carries
    /// the coverage it had and no value at all.
    #[test]
    fn a_metric_under_the_minimum_is_missing_and_never_filled() {
        let samples = [
            at_minute("hall.kw", 0, 100.0, Unit::Kilowatts),
            at_minute("hall.kw", 3, 110.0, Unit::Kilowatts),
            at_minute("hall.kw", 6, 120.0, Unit::Kilowatts),
        ];

        let aligned = align(&samples, interval(), &rules()).expect("three samples align");

        assert_eq!(
            aligned.get(&metric("hall.kw")),
            Some(&MetricReading::Missing {
                coverage: Percent::new(40.0).expect("a percentage")
            })
        );
        assert_eq!(
            aligned.missing().collect::<Vec<_>>(),
            vec![(&metric("hall.kw"), Percent::new(40.0).expect("a percent"))]
        );
    }

    /// One sample spans nothing, so there is no mean to take, whatever the
    /// configured minimum says.
    #[test]
    fn one_sample_covers_nothing_even_when_nothing_is_required() {
        let permissive = AlignRules::new(Percent::zero(), Vec::new());
        let samples = [at_minute("hall.kw", 4, 100.0, Unit::Kilowatts)];

        let aligned = align(&samples, interval(), &permissive).expect("one sample aligns");

        assert_eq!(
            aligned.get(&metric("hall.kw")),
            Some(&MetricReading::Missing {
                coverage: Percent::zero()
            })
        );
    }

    #[test]
    fn a_duplicate_instant_keeps_the_later_value_and_is_counted() {
        let samples = [
            at_minute("hall.kw", 0, 100.0, Unit::Kilowatts),
            at_minute("hall.kw", 5, 999.0, Unit::Kilowatts),
            at_minute("hall.kw", 5, 200.0, Unit::Kilowatts),
            at_minute("hall.kw", 12, 100.0, Unit::Kilowatts),
        ];

        let aligned = align(&samples, interval(), &rules()).expect("four samples align");

        let hall = present(&aligned, "hall.kw");
        assert_eq!(hall.dupes(), 1);
        assert_eq!(aligned.dupes(), 1);
        assert!(
            (hall.value() - 158.333_333_333_333_33).abs() < 1e-9,
            "the replayed 999 survived: {}",
            hall.value()
        );
    }

    #[test]
    fn a_declared_counter_aligns_to_its_rise() {
        let counter = metric("meter.main.import_kw");
        let declared =
            AlignRules::new(Percent::new(50.0).expect("a percentage"), [counter.clone()]);
        let samples = [
            at_minute("meter.main.import_kw", 0, 1_000_000.0, Unit::KilowattHours),
            at_minute("meter.main.import_kw", 7, 1_000_182.0, Unit::KilowattHours),
            at_minute("meter.main.import_kw", 14, 1_000_364.0, Unit::KilowattHours),
        ];

        let aligned = align(&samples, interval(), &declared).expect("a counter aligns");

        let rise = present(&aligned, "meter.main.import_kw");
        assert!((rise.value() - 364.0).abs() < f64::EPSILON);
    }

    /// The same samples, undeclared, are a summed energy feed. Which rule
    /// applies is the declaration's to say and never the data's.
    #[test]
    fn the_same_samples_undeclared_are_summed() {
        let samples = [
            at_minute("meter.main.import_kw", 0, 10.0, Unit::KilowattHours),
            at_minute("meter.main.import_kw", 7, 20.0, Unit::KilowattHours),
        ];

        let aligned = align(&samples, interval(), &rules()).expect("an energy feed aligns");

        let summed = present(&aligned, "meter.main.import_kw");
        assert!((summed.value() - 30.0).abs() < f64::EPSILON);
    }

    /// A summed feed has no gap between rows to measure, so one row covers the
    /// interval and none leaves the metric absent rather than thin.
    #[test]
    fn one_summed_row_covers_the_interval() {
        let samples = [at_minute("meter.main.kwh", 9, 42.0, Unit::KilowattHours)];

        let aligned = align(&samples, interval(), &rules()).expect("one row aligns");

        let row = present(&aligned, "meter.main.kwh");
        assert_eq!(row.coverage(), Percent::new(100.0).expect("a percentage"));
        assert!((row.value() - 42.0).abs() < f64::EPSILON);
    }

    #[test]
    fn a_metric_with_no_sample_is_absent_rather_than_missing() {
        let samples = [at_minute("hall.kw", 0, 100.0, Unit::Kilowatts)];

        let aligned = align(&samples, interval(), &rules()).expect("one sample aligns");

        assert_eq!(aligned.get(&metric("cooling.kw")), None);
        assert_eq!(aligned.len(), 1);
    }

    #[test]
    fn an_empty_interval_holds_no_metric_at_all() {
        let aligned = align(&[], interval(), &rules()).expect("nothing aligns to nothing");

        assert!(aligned.is_empty());
        assert_eq!(aligned.interval(), interval());
    }

    #[test]
    fn a_sample_from_another_quarter_hour_is_refused() {
        let samples = [at_minute("hall.kw", 15, 100.0, Unit::Kilowatts)];

        let refused = align(&samples, interval(), &rules());

        assert_eq!(
            refused,
            Err(AlignError::SampleOutsideInterval {
                metric: metric("hall.kw"),
                at: (FIXTURE_EPOCH_MIN + 15) * 60,
            })
        );
    }

    #[test]
    fn a_metric_that_changes_unit_is_refused() {
        let samples = [
            at_minute("hall.kw", 0, 100.0, Unit::Kilowatts),
            at_minute("hall.kw", 5, 100_000.0, Unit::Watts),
        ];

        let refused = align(&samples, interval(), &rules());

        assert_eq!(
            refused,
            Err(AlignError::UnitChanged {
                metric: metric("hall.kw")
            })
        );
    }

    #[test]
    fn a_counter_that_falls_is_refused() {
        let counter = metric("meter.main.import_kw");
        let declared = AlignRules::new(Percent::zero(), [counter.clone()]);
        let samples = [
            at_minute("meter.main.import_kw", 0, 1_000_000.0, Unit::KilowattHours),
            at_minute("meter.main.import_kw", 9, 7.0, Unit::KilowattHours),
        ];

        let refused = align(&samples, interval(), &declared);

        assert_eq!(
            refused,
            Err(AlignError::CounterWentBackwards { metric: counter })
        );
    }

    #[test]
    fn watts_and_kilowatts_default_to_a_mean_and_energy_to_a_sum() {
        let rules = rules();

        assert_eq!(
            rules.aggregation_for(&metric("gpu.power_w"), Unit::Watts),
            Aggregation::TimeWeightedMean
        );
        assert_eq!(
            rules.aggregation_for(&metric("hall.kw"), Unit::Kilowatts),
            Aggregation::TimeWeightedMean
        );
        assert_eq!(
            rules.aggregation_for(&metric("meter.main.kwh"), Unit::KilowattHours),
            Aggregation::Sum
        );
    }

    #[test]
    fn a_declared_counter_overrides_the_units_default() {
        let counter = metric("meter.main.import_kw");
        let declared = AlignRules::new(Percent::zero(), [counter.clone()]);

        assert_eq!(
            declared.aggregation_for(&counter, Unit::KilowattHours),
            Aggregation::CounterDelta
        );
    }

    #[test]
    fn the_default_rules_carry_the_estimate_minimum() {
        assert_eq!(
            rules().min_coverage(),
            Percent::new(AlignRules::DEFAULT_MIN_COVERAGE_PCT).expect("a percentage")
        );
    }

    #[test]
    fn metrics_are_read_in_name_order_whatever_order_they_arrived_in() {
        let samples = [
            at_minute("pdu.a1.kw", 0, 400.0, Unit::Kilowatts),
            at_minute("cooling.kw", 0, 200.0, Unit::Kilowatts),
            at_minute("pdu.a1.kw", 14, 400.0, Unit::Kilowatts),
            at_minute("cooling.kw", 14, 200.0, Unit::Kilowatts),
        ];

        let aligned = align(&samples, interval(), &rules()).expect("two metrics align");

        let names: Vec<&str> = aligned
            .present()
            .map(|(metric, _)| metric.as_str())
            .collect();
        assert_eq!(names, ["cooling.kw", "pdu.a1.kw"]);
    }

    /// A reading's coverage can be read without knowing whether it was good
    /// enough, which is what the day report needs.
    #[test]
    fn a_reading_reports_its_coverage_either_way() {
        let thin = MetricReading::Missing {
            coverage: Percent::new(40.0).expect("a percentage"),
        };
        let full = MetricReading::Present(Reading {
            value: 1.0,
            unit: Unit::Kilowatts,
            coverage: Percent::new(100.0).expect("a percentage"),
            dupes: 0,
        });

        assert_eq!(thin.coverage(), Percent::new(40.0).expect("a percentage"));
        assert_eq!(full.coverage(), Percent::new(100.0).expect("a percentage"));
    }
}
