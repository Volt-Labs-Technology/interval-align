//! Consumer contract: alignment types and golden numbers must not drift.
//! All timestamps and values are synthetic.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use interval_align::{
    align, AlignError, Duration, Interval, Kind, Reading, Rules, Sample, Timestamp,
};

type AlignResult = Result<BTreeMap<String, Reading>, AlignError>;

const _: fn(Timestamp) -> i64 = timestamp_as_i64;
const _: fn(&[Sample], Interval, &Rules) -> AlignResult = align;

/// 2026-03-08T06:00Z as Unix seconds. Synthetic; this crate reads no clock.
const START: Timestamp = 29_549_160 * 60;
/// 2026-11-01T05:00Z as Unix seconds. Synthetic; derived by hand, not a zone db.
const AUTUMN_START: Timestamp = 29_891_820 * 60;

fn timestamp_as_i64(at: Timestamp) -> i64 {
    at
}

fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Power => "Power",
        Kind::Energy => "Energy",
        Kind::Counter => "Counter",
    }
}

fn default_length() -> Duration {
    Duration::seconds(900).expect("a positive length")
}

fn default_interval() -> Interval {
    Interval {
        start: START,
        length: default_length(),
    }
}

fn default_rules() -> Rules {
    Rules {
        min_coverage_pct: 80,
    }
}

fn sample(metric: &str, at: Timestamp, value: f64, kind: Kind) -> Sample {
    Sample::new(metric, at, value, kind).expect("a finite value")
}

fn walk_starts(start: Timestamp, window_secs: i64) -> Vec<Timestamp> {
    let length = default_length();
    let end = start + window_secs;
    let mut starts = Vec::new();
    let mut next = start;
    while next < end {
        let interval = Interval {
            start: next,
            length,
        };
        starts.push(interval.start);
        next += i64::try_from(length.as_secs()).expect("length fits i64");
    }
    starts
}

fn assert_strictly_stepping(starts: &[Timestamp], step: i64) {
    for pair in starts.windows(2) {
        assert_eq!(pair[1] - pair[0], step);
        assert!(pair[1] > pair[0]);
    }
}

fn dependency_crate_names(cargo_toml: &str) -> Vec<&str> {
    let section = cargo_toml
        .split("[dependencies]")
        .nth(1)
        .expect("[dependencies]");
    let section = section.split("\n[").next().unwrap_or(section);
    section
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            line.split([' ', '=', '.']).next()
        })
        .collect()
}

fn collect_rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("src directory") {
        let path = entry.expect("directory entry").path();
        if path.is_dir() {
            collect_rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn contains_token(haystack: &str, needle: &str) -> bool {
    haystack.match_indices(needle).any(|(index, _)| {
        let before_ok = index == 0 || !is_ident_char(haystack.as_bytes()[index - 1]);
        let after = index + needle.len();
        let after_ok = after == haystack.len() || !is_ident_char(haystack.as_bytes()[after]);
        before_ok && after_ok
    })
}

fn src_contains_forbidden(text: &str) -> Option<&'static str> {
    const TOKENS: &[&str] = &["chrono", "HashMap", "interpolate", "tz", "SystemTime"];
    const PATHS: &[&str] = &["time::", "Utc::now"];
    TOKENS
        .iter()
        .copied()
        .find(|token| contains_token(text, token))
        .or_else(|| PATHS.iter().copied().find(|path| text.contains(path)))
}

#[test]
fn kinds_are_power_energy_and_counter() {
    assert_eq!(kind_name(Kind::Power), "Power");
    assert_eq!(kind_name(Kind::Energy), "Energy");
    assert_eq!(kind_name(Kind::Counter), "Counter");
}

#[test]
fn timestamp_is_i64() {
    let at: Timestamp = START;
    let as_i64: i64 = timestamp_as_i64(at);
    assert_eq!(as_i64, 29_549_160 * 60);
}

#[test]
fn power_hand_example_is_the_time_weighted_mean() {
    let samples = [
        sample("hall.kw", START, 100.0, Kind::Power),
        sample("hall.kw", START + 300, 200.0, Kind::Power),
        sample("hall.kw", START + 720, 100.0, Kind::Power),
    ];

    let aligned: BTreeMap<String, Reading> =
        align(&samples, default_interval(), &default_rules()).expect("three samples align");

    match aligned.get("hall.kw").expect("hall.kw was sampled") {
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
            panic!("expected Value, got Missing {{ coverage_pct: {coverage_pct} }}")
        }
    }
}

#[test]
fn thin_power_is_missing_with_no_value_field() {
    let samples = [
        sample("hall.kw", START, 100.0, Kind::Power),
        sample("hall.kw", START + 180, 110.0, Kind::Power),
        sample("hall.kw", START + 360, 120.0, Kind::Power),
    ];

    let aligned: BTreeMap<String, Reading> =
        align(&samples, default_interval(), &default_rules()).expect("three samples align");

    match aligned.get("hall.kw").expect("hall.kw was sampled") {
        Reading::Missing { coverage_pct } => assert_eq!(*coverage_pct, 40),
        Reading::Value {
            value,
            coverage_pct,
            duplicates,
        } => panic!(
            "expected Missing with no value, got Value {{ value: {value}, coverage_pct: {coverage_pct}, duplicates: {duplicates} }}"
        ),
    }
}

#[test]
fn replay_before_the_kept_sample_counts_as_one_duplicate() {
    let samples = [
        sample("hall.kw", START, 100.0, Kind::Power),
        sample("hall.kw", START + 300, 999.0, Kind::Power),
        sample("hall.kw", START + 300, 200.0, Kind::Power),
        sample("hall.kw", START + 720, 100.0, Kind::Power),
    ];

    let aligned: BTreeMap<String, Reading> =
        align(&samples, default_interval(), &default_rules()).expect("four samples align");

    match aligned.get("hall.kw").expect("hall.kw was sampled") {
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
            panic!("expected Value, got Missing {{ coverage_pct: {coverage_pct} }}")
        }
    }
}

#[test]
fn utc_interval_walks_are_ninety_two_and_one_hundred_starts() {
    let spring = walk_starts(START, 23 * 3_600);
    assert_eq!(spring.len(), 92);
    assert_strictly_stepping(&spring, 900);
    assert_eq!(spring[0], START);

    let autumn = walk_starts(AUTUMN_START, 25 * 3_600);
    assert_eq!(autumn.len(), 100);
    assert_strictly_stepping(&autumn, 900);
    assert_eq!(autumn[0], AUTUMN_START);
}

#[test]
fn one_power_sample_is_missing_even_at_zero_minimum() {
    let samples = [sample("hall.kw", START, 100.0, Kind::Power)];
    let rules = Rules {
        min_coverage_pct: 0,
    };

    let aligned: BTreeMap<String, Reading> =
        align(&samples, default_interval(), &rules).expect("one sample aligns");

    match aligned.get("hall.kw").expect("hall.kw was sampled") {
        Reading::Missing { coverage_pct } => assert_eq!(*coverage_pct, 0),
        Reading::Value {
            value,
            coverage_pct,
            duplicates,
        } => panic!(
            "expected Missing with no value, got Value {{ value: {value}, coverage_pct: {coverage_pct}, duplicates: {duplicates} }}"
        ),
    }
}

#[test]
fn one_energy_sample_is_the_value_itself() {
    let samples = [sample("meter.kwh", START, 42.0, Kind::Energy)];

    let aligned: BTreeMap<String, Reading> =
        align(&samples, default_interval(), &default_rules()).expect("one row aligns");

    match aligned.get("meter.kwh").expect("meter.kwh was sampled") {
        Reading::Value {
            value,
            coverage_pct,
            duplicates: _,
        } => {
            assert_eq!(*value, 42.0);
            assert_eq!(*coverage_pct, 100);
        }
        Reading::Missing { coverage_pct } => {
            panic!("expected Value, got Missing {{ coverage_pct: {coverage_pct} }}")
        }
    }
}

#[test]
fn counter_rise_is_the_difference() {
    let samples = [
        sample("meter.total", START, 1_000_000.0, Kind::Counter),
        sample("meter.total", START + 540, 1_000_364.0, Kind::Counter),
    ];
    let rules = Rules {
        min_coverage_pct: 50,
    };

    let aligned: BTreeMap<String, Reading> =
        align(&samples, default_interval(), &rules).expect("a counter aligns");

    match aligned.get("meter.total").expect("meter.total was sampled") {
        Reading::Value {
            value,
            coverage_pct: _,
            duplicates: _,
        } => assert_eq!(*value, 364.0),
        Reading::Missing { coverage_pct } => {
            panic!("expected Value, got Missing {{ coverage_pct: {coverage_pct} }}")
        }
    }
}

#[test]
fn falling_counter_is_counter_went_backwards() {
    let samples = [
        sample("meter.total", START, 1_000_000.0, Kind::Counter),
        sample("meter.total", START + 720, 7.0, Kind::Counter),
    ];

    let refused = align(&samples, default_interval(), &default_rules());

    assert_eq!(
        refused,
        Err(AlignError::CounterWentBackwards {
            metric: "meter.total".to_owned()
        })
    );
}

#[test]
fn sample_at_interval_end_is_outside() {
    let samples = [sample("hall.kw", START + 900, 100.0, Kind::Power)];

    let refused = align(&samples, default_interval(), &default_rules());

    assert_eq!(
        refused,
        Err(AlignError::SampleOutsideInterval {
            metric: "hall.kw".to_owned(),
            at: START + 900,
        })
    );
}

#[test]
fn zero_duration_and_nan_sample_are_none() {
    assert!(Duration::seconds(0).is_none());
    assert!(Sample::new("hall.kw", START, f64::NAN, Kind::Power).is_none());
}

#[test]
fn unsampled_metric_is_absent_not_missing() {
    let samples = [
        sample("hall.kw", START, 100.0, Kind::Power),
        sample("hall.kw", START + 300, 200.0, Kind::Power),
        sample("hall.kw", START + 720, 100.0, Kind::Power),
    ];

    let aligned: BTreeMap<String, Reading> =
        align(&samples, default_interval(), &default_rules()).expect("three samples align");

    assert_eq!(aligned.get("never.sampled"), None);
    assert!(!matches!(
        aligned.get("never.sampled"),
        Some(Reading::Missing { .. })
    ));
}

#[test]
fn cargo_toml_runtime_dependencies_are_only_thiserror() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let text = fs::read_to_string(&path).expect("Cargo.toml");
    let names = dependency_crate_names(&text);
    assert_eq!(names, ["thiserror"]);
}

#[test]
fn src_does_not_use_forbidden_time_or_hashmap_words() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect_rust_files(&src, &mut files);
    assert!(!files.is_empty(), "expected src/**/*.rs");

    for path in files {
        let text = fs::read_to_string(&path).expect("source file");
        if let Some(needle) = src_contains_forbidden(&text) {
            panic!("{} contains {needle}", path.display());
        }
    }
}
