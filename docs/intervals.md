# Quarter-hours: how samples become one number, and what "missing" means

A hall bills on 15-minute intervals and the loops decide on them, so every
feed Eclairos reads ends up as one number per metric per quarter-hour. That
step is one calculation, `eclairos_domain::align`, and this document is what it
does and refuses to do. `crates/eclairos-domain/src/align.rs` is the code;
nothing else in the workspace turns samples into intervals.

## A demand interval is a mean, not a maximum

**The peak that sets a demand charge is the highest 15-minute *mean*, not the
highest instantaneous reading.** A hall that touches 1.9 MW for four seconds
and averages 1.28 MW over the quarter-hour was billed on 1.28 MW. Aligning a
power metric to its maximum would invent a peak the meter never recorded and a
charge the utility never made.

So power metrics — anything in watts or kilowatts — align to the interval's
**time-weighted mean**: each sample is weighted by the span until the next
one.

| minute | value | weighted for |
|---|---|---|
| 0 | 100 kW | 5 minutes |
| 5 | 200 kW | 7 minutes |
| 12 | 100 kW | — |

`(100 × 5 + 200 × 7) / 12 = 158.33 kW`. The last sample opens no span and so
carries no weight. Nothing is held past it and nothing is back-filled before
the first sample: an unsampled minute is lost coverage, never a value.

## Coverage is the sampled span

A feed does not declare its rate, so "how much of this quarter-hour did we
actually see?" cannot be a fraction of an expected number of rows. It is the
span from the first sample in the interval to the last, over the interval's
fifteen minutes.

- Samples over the first six minutes → **40 %**.
- Samples at minutes 0, 5 and 12 → **80 %**.
- One sample a minute for fifteen minutes → **93.3 %**, because the last
  minute opens no span. That is the rule, not a rounding error.
- One sample alone → **0 %**. One sample is not a mean.

A metric whose coverage is below the configured minimum is **`Missing`**. A
missing reading carries the coverage it did have and **no value** — there is
no field to read "anyway", which is what keeps a thin interval out of a
ledger. The minimum defaults to **80 %, an ESTIMATE**: three missed minutes
out of fifteen. A site's own figure comes from its config file (E4.3).

**Nothing is interpolated, ever.** Not a gap, not a dropped feed, not a
restarted collector. A number Eclairos publishes was measured or it is not
there.

## Three aggregations, and each one is declared

| the metric is | aggregated as | coverage rule |
|---|---|---|
| a power (`W`, `kW`) | time-weighted mean | the sampled span |
| an energy row (`kWh`) | sum | present from one row, absent with none |
| a monotone counter (`kWh` total) | last minus first | the sampled span |

A `kWh` sample does not say whether it is this interval's energy or a meter's
running total. OpenTelemetry's gauge-versus-sum distinction is a *wire*
concept and stops at the adapter, so by the time the domain sees a `Sample`
both look the same. **Which metrics are counters is therefore declared** in
`AlignRules`, and everything else takes the default for its unit. Guessing
from monotonicity would silently mis-bill the first interval of a meter that
happened to rise. `AlignRules::aggregation_for` is the one place this is
decided.

A summed feed's coverage works differently on purpose: a 15-minute meter row
*is* the interval's energy, stamped once inside it, and there is no gap
between rows to measure. So one row covers the interval and no rows leaves the
metric **absent** from the reading rather than missing from it — nothing was
expected of it, and an expectation is configuration's to hold.

**A counter's reading is the rise between the first and last sample inside the
interval.** It is not the quarter-hour's true energy: closing that boundary
needs the *next* interval's first reading, which the alignment of one interval
does not have and must not reach for. The fold thread (E3.5) owns that
crossing. Do not reconcile a counter series against a meter invoice and blame
the arithmetic.

## Duplicates: the later sample wins, and the count is kept

Two samples of one metric at one instant is a feed that replayed, not a
measurement that happened twice. The later one wins and the discard is counted
in `dupes`, which the day report carries — so a replaying feed is visible
rather than smoothed away.

## What alignment refuses

Three things are typed errors rather than rows quietly dropped:

- **A sample from another quarter-hour.** `Interval15::containing` is the one
  conversion and the caller has already used it, so this is a bug in the
  caller. Ignoring it would hide the bug and move the mean.
- **A metric that changes unit inside one interval** — kW to W halfway
  through is a decoding mistake, not a measurement.
- **A declared counter that falls.** A meter that rolled over or reset needs a
  person, not an arithmetic guess.

## The day report

`eclairos_domain::validate_day` judges a day of readings: how many
quarter-hours there were, how many were complete, how many intervals each
metric went missing in, how many duplicates were discarded, the **first three**
offending metric-and-interval pairs, and every quarter-hour whose hall mean
read above the site's rating by more than **5 %** (`RATING_TOLERANCE`, E1.5).

**An over-rating quarter-hour is reported with the megawatts that were
measured and is never clipped to the rating.** Clipping would turn a wrong
number into a plausible one, and the wrong number is the finding — either the
metering is off or the nameplate is.

Every report carries a caption and there is no way to build one without it:
`Laptop fixture · not a hall` or `Shadow · <alias> · no writes`, and no third.
`eclairos_ingest::validate` writes the JSON; every rule above stays in the
domain, so the CLI's `shadow verify` (E4.3c) and the shadow-month report
cannot disagree with it about the same day.

## Days are UTC days

Intervals are UTC, as `Interval15::month_of` already says. A site's local day
is **not** used to group them, because deriving a local midnight needs a
time-zone database and Eclairos carries no such dependency. `TimeZoneName` is
the name a site's config supplies for the tariff's time-of-use periods (E1.1),
which is where local time genuinely belongs.

This costs nothing at a daylight-saving change, and
`crates/eclairos-domain/tests/dst_intervals.rs` is the proof: a 23-hour local
day is 92 whole quarter-hours and a 25-hour local day is 100, both with no
gap, no duplicate and no partial interval. Alignment never has to know that a
clock moved.
