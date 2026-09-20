# interval-align

## What this is

A small, dependency-light Rust crate that aligns timestamped samples onto intervals and returns one number per metric: a time-weighted mean for power-like metrics, a sum for energy rows, or a rise for monotone counters — each with a coverage figure and nothing interpolated.

## Who it is for

Rust programs that ingest metering or monitoring feeds at irregular rates and need to summarize them over fixed intervals without inventing data.

## How to use it

Add the crate to a Rust project:

```sh
cargo add interval-align
```

That writes this line into `Cargo.toml`:

```toml
interval-align = "0.1.0"
```

Minimal example (synthetic data):

```rust
use std::collections::BTreeMap;

use interval_align::{align, Duration, Interval, Kind, Reading, Rules, Sample};

// Timestamps are seconds since the Unix epoch, as `i64`.
let start = 1_772_949_600;
let interval = Interval {
    start,
    length: Duration::seconds(900).expect("a positive length"),
};

// Fifteen minutes is only this example's length. Any positive length works.
let samples = [
    Sample::new("hall.kw", start, 100.0, Kind::Power).expect("finite"),
    Sample::new("hall.kw", start + 300, 200.0, Kind::Power).expect("finite"),
    Sample::new("hall.kw", start + 720, 100.0, Kind::Power).expect("finite"),
];

let readings: BTreeMap<String, Reading> =
    align(&samples, interval, &Rules { min_coverage_pct: 80 }).expect("all samples inside");

// hall.kw was held at 100 for five minutes, then 200 for seven, then 100
// until the end. The time-weighted mean is 158.333… over 80 % coverage.
assert!(matches!(
    readings["hall.kw"],
    Reading::Value { coverage_pct: 80, .. }
));
```

The rules in one place:

* A sample declares its `Kind`; nothing is inferred from a unit string.
  `Kind::Power` is the interval's time-weighted mean (each sample held until
  the next; the last opens no span), `Kind::Energy` is the sum of the values
  inside the interval, and `Kind::Counter` is the last value minus the first.
* Coverage for power and counter metrics is the span from the first sample
  to the last, over the interval's length, as a percent. Fewer than two
  samples means zero coverage. One energy sample covers the whole interval.
* Coverage below the configured minimum, or zero coverage, yields
  `Reading::Missing { coverage_pct }` and no value. Nothing is interpolated
  or filled, ever.
* Two samples of one metric at one instant is a replay: the later one in
  caller order wins, and the discard is counted in `duplicates`.
* A sample outside the interval, or a counter that falls, is a typed error
  rather than a row quietly dropped.
* A metric with no samples at all is absent from the returned map rather
  than `Missing`. The map is a `BTreeMap`, so the same input always yields
  the same iteration order.

## What it deliberately does not do

This crate deliberately does not interpolate between samples, convert time zones, read files or the network, or decide what a good coverage threshold is — a caller supplies `min_coverage_pct`, and its own meaning of a local day. Timestamps are plain seconds since the Unix epoch as `i64`; the crate does no time-zone work, never reads a clock, and treats fifteen minutes as an example interval length, not a built-in.

Version 0.x: the API may change before 1.0.
