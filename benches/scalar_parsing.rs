use core::hint::black_box;
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use sql_scalar_text::{
    parse_bool, parse_date, parse_f64, parse_i64, parse_interval, parse_pg_bytea_hex, parse_time,
    parse_timestamp, parse_timestamp_tz,
};

#[cfg(feature = "decimal")]
use sql_scalar_text::parse_decimal;

fn timestamp_corpus() -> Vec<&'static str> {
    vec![
        "2026-01-01 00:00:00",
        "2026-01-01T00:00:00",
        "2026-01-01 00:00:00.5",
        "2026-01-01T00:00:00.5",
    ]
}

fn timestamp_tz_corpus() -> Vec<&'static str> {
    vec![
        "2026-01-01 00:00:00+00",
        "2026-01-01 00:00:00+00:00",
        "2026-01-01 00:00:00+0000",
        "2026-01-01 00:00:00Z",
        "2026-01-01T00:00:00Z",
        "2026-01-01T00:00:00+00:00",
        "2025-12-31 22:00:00-02",
        "2026-01-01 02:30:00+02:30",
        "2026-01-01 00:00:00.5+00",
        "2026-01-01T00:00:00.5Z",
    ]
}

fn date_corpus() -> Vec<&'static str> {
    vec!["2026-01-01"]
}

fn time_corpus() -> Vec<&'static str> {
    vec!["00:00:00", "12:34:56.789"]
}

fn bool_corpus() -> Vec<&'static str> {
    vec!["t", "f", "1", "0"]
}

fn bytea_corpus() -> Vec<&'static str> {
    vec![r"\x", r"\x00", r"\x0001ff", r"\xABCDEF"]
}

fn i64_corpus() -> Vec<&'static str> {
    vec![
        "0",
        "-1",
        "+1",
        "9223372036854775807",
        "-9223372036854775808",
    ]
}

fn f64_corpus() -> Vec<&'static str> {
    vec![
        "0",
        "-1.5",
        "1e20",
        "3.141592653589793",
        "Infinity",
        "-Infinity",
    ]
}

fn interval_corpus() -> Vec<&'static str> {
    vec![
        "00:15:00",
        "1 day",
        "1 year 2 mons 3 days 04:05:06.789",
        "-1 days +02:00:00",
        "@ 1 hour ago",
        "PT15M",
    ]
}

#[cfg(feature = "decimal")]
fn decimal_corpus() -> Vec<&'static str> {
    vec!["0", "-1.5", "+1", "1e20", "12345678901234567890.123456789"]
}

fn bench_parse_timestamp(c: &mut Criterion) {
    let corpus = timestamp_corpus();
    let mut group = c.benchmark_group("parse_timestamp");
    for &input in &corpus {
        group.bench_with_input(BenchmarkId::from_parameter(input), &input, |b, text| {
            b.iter(|| black_box(parse_timestamp(black_box(*text))))
        });
    }
    group.finish();
}

fn bench_parse_timestamp_tz(c: &mut Criterion) {
    let corpus = timestamp_tz_corpus();
    let mut group = c.benchmark_group("parse_timestamp_tz");
    for &input in &corpus {
        group.bench_with_input(BenchmarkId::from_parameter(input), &input, |b, text| {
            b.iter(|| black_box(parse_timestamp_tz(black_box(*text))))
        });
    }
    group.finish();
}

fn bench_parse_date(c: &mut Criterion) {
    let corpus = date_corpus();
    let mut group = c.benchmark_group("parse_date");
    for &input in &corpus {
        group.bench_with_input(BenchmarkId::from_parameter(input), &input, |b, text| {
            b.iter(|| black_box(parse_date(black_box(*text))))
        });
    }
    group.finish();
}

fn bench_parse_time(c: &mut Criterion) {
    let corpus = time_corpus();
    let mut group = c.benchmark_group("parse_time");
    for &input in &corpus {
        group.bench_with_input(BenchmarkId::from_parameter(input), &input, |b, text| {
            b.iter(|| black_box(parse_time(black_box(*text))))
        });
    }
    group.finish();
}

fn bench_parse_bool(c: &mut Criterion) {
    let corpus = bool_corpus();
    let mut group = c.benchmark_group("parse_bool");
    for &input in &corpus {
        group.bench_with_input(BenchmarkId::from_parameter(input), &input, |b, text| {
            b.iter(|| black_box(parse_bool(black_box(*text))))
        });
    }
    group.finish();
}

fn bench_parse_pg_bytea_hex(c: &mut Criterion) {
    let corpus = bytea_corpus();
    let mut group = c.benchmark_group("parse_pg_bytea_hex");
    for &input in &corpus {
        group.bench_with_input(BenchmarkId::from_parameter(input), &input, |b, text| {
            b.iter(|| black_box(parse_pg_bytea_hex(black_box(*text))))
        });
    }
    group.finish();
}

fn bench_parse_i64(c: &mut Criterion) {
    let corpus = i64_corpus();
    let mut group = c.benchmark_group("parse_i64");
    for &input in &corpus {
        group.bench_with_input(BenchmarkId::from_parameter(input), &input, |b, text| {
            b.iter(|| black_box(parse_i64(black_box(*text))))
        });
    }
    group.finish();
}

fn bench_parse_f64(c: &mut Criterion) {
    let corpus = f64_corpus();
    let mut group = c.benchmark_group("parse_f64");
    for &input in &corpus {
        group.bench_with_input(BenchmarkId::from_parameter(input), &input, |b, text| {
            b.iter(|| black_box(parse_f64(black_box(*text))))
        });
    }
    group.finish();
}

fn bench_parse_interval(c: &mut Criterion) {
    let corpus = interval_corpus();
    let mut group = c.benchmark_group("parse_interval");
    for &input in &corpus {
        group.bench_with_input(BenchmarkId::from_parameter(input), &input, |b, text| {
            b.iter(|| black_box(parse_interval(black_box(*text))))
        });
    }
    group.finish();
}

#[cfg(feature = "decimal")]
fn bench_parse_decimal(c: &mut Criterion) {
    let corpus = decimal_corpus();
    let mut group = c.benchmark_group("parse_decimal");
    for &input in &corpus {
        group.bench_with_input(BenchmarkId::from_parameter(input), &input, |b, text| {
            b.iter(|| black_box(parse_decimal(black_box(*text))))
        });
    }
    group.finish();
}

#[cfg(feature = "decimal")]
criterion_group!(
    benches,
    bench_parse_timestamp,
    bench_parse_timestamp_tz,
    bench_parse_date,
    bench_parse_time,
    bench_parse_bool,
    bench_parse_pg_bytea_hex,
    bench_parse_i64,
    bench_parse_f64,
    bench_parse_interval,
    bench_parse_decimal,
);

#[cfg(not(feature = "decimal"))]
criterion_group!(
    benches,
    bench_parse_timestamp,
    bench_parse_timestamp_tz,
    bench_parse_date,
    bench_parse_time,
    bench_parse_bool,
    bench_parse_pg_bytea_hex,
    bench_parse_i64,
    bench_parse_f64,
    bench_parse_interval,
);

criterion_main!(benches);
