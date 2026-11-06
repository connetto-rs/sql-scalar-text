use core::fmt::Write as _;
use proptest::prelude::*;
use sql_values::{
    PgDate, PgInterval, PgTime, PgTimestamp, PgTimestampTz, parse_bool, parse_date, parse_f64,
    parse_i64, parse_interval, parse_pg_bytea_hex, parse_time, parse_timestamp, parse_timestamp_tz,
};
use std::hash::BuildHasher as _;

/// The day, days from 2000-01-01, of 4714-11-24 BC, the first date PostgreSQL holds.
const MIN_PG_DAYS: i32 = -2_451_545;
/// The day, days from 2000-01-01, of 5874897-12-31, the last date PostgreSQL holds.
const MAX_PG_DAYS: i32 = 2_145_031_948;
/// The microsecond, from 2000-01-01, of 4714-11-24 00:00:00 BC, the first timestamp.
const MIN_TS_MICROS: i64 = -211_813_488_000_000_000;
/// The microsecond, from 2000-01-01, of 294276-12-31 23:59:59.999999, the last timestamp.
const MAX_TS_MICROS: i64 = 9_223_371_331_199_999_999;

fn date_strategy() -> impl Strategy<Value = PgDate> {
    (1970i32..=2100, 1u8..=12, 1u8..=31)
        .prop_filter_map("valid calendar date", |(year, month, day)| {
            PgDate::from_ymd(year, month, day)
        })
}

fn full_range_date_strategy() -> impl Strategy<Value = PgDate> {
    (MIN_PG_DAYS..=MAX_PG_DAYS).prop_map(|days| PgDate::from_pg_days(days).unwrap())
}

fn time_strategy() -> impl Strategy<Value = PgTime> {
    (0i64..=86_400_000_000).prop_map(|micros| PgTime::from_micros(micros).unwrap())
}

fn datetime_strategy() -> impl Strategy<Value = PgTimestamp> {
    (date_strategy(), time_strategy())
        .prop_map(|(date, time)| PgTimestamp::new(date, time).unwrap())
}

fn full_range_timestamp_strategy() -> impl Strategy<Value = PgTimestamp> {
    (MIN_TS_MICROS..=MAX_TS_MICROS).prop_map(|micros| PgTimestamp::from_pg_micros(micros).unwrap())
}

fn offset_secs_strategy() -> impl Strategy<Value = i32> {
    (-1439i32..=1439).prop_map(|m| m * 60)
}

fn second_offset_secs_strategy() -> impl Strategy<Value = i32> {
    -86_399i32..=86_399
}

fn whole_hour_offset_secs_strategy() -> impl Strategy<Value = i32> {
    (-23i32..=23).prop_map(|h| h * 3600)
}

/// The local clock `offset` seconds east of the UTC instant `utc`, as a plain timestamp.
fn fmt_local(utc: PgTimestamp, offset: i32) -> PgTimestamp {
    PgTimestamp::from_pg_micros(utc.pg_micros() + i64::from(offset) * 1_000_000).unwrap()
}

fn offset_parts(offset: i32) -> (char, u32, u32, u32) {
    let sign = if offset >= 0 { '+' } else { '-' };
    let abs = offset.unsigned_abs();
    let h = abs / 3600;
    let m = (abs % 3600) / 60;
    let s = abs % 60;
    (sign, h, m, s)
}

proptest! {
    #[test]
    fn timestamp_pg_layout_roundtrip(dt in datetime_strategy()) {
        let text = format!("{} {}", dt.date(), dt.time().unwrap());
        let got = parse_timestamp(&text)
            .unwrap_or_else(|| panic!("parse_timestamp: {text:?}"));
        prop_assert_eq!(got, dt);
    }

    #[test]
    fn timestamp_t_separator_roundtrip(dt in datetime_strategy()) {
        let text = format!("{}T{}", dt.date(), dt.time().unwrap());
        let got = parse_timestamp(&text)
            .unwrap_or_else(|| panic!("parse_timestamp T: {text:?}"));
        prop_assert_eq!(got, dt);
    }

    #[test]
    fn timestamp_tz_z_roundtrip(dt in datetime_strategy()) {
        let local = dt.to_string();
        let text = format!("{local}Z");
        let got = parse_timestamp_tz(&text)
            .unwrap_or_else(|| panic!("parse_timestamp_tz Z: {text:?}"));
        prop_assert_eq!(got, PgTimestampTz::from_pg_micros(dt.pg_micros()).unwrap());
    }

    #[test]
    fn timestamp_tz_plus_hh_roundtrip(dt in datetime_strategy(), offset in whole_hour_offset_secs_strategy()) {
        let local = fmt_local(dt, offset);
        let (sign, h, _m, _s) = offset_parts(offset);
        let text = format!("{} {time}{sign}{h:02}", local.date(), time = local.time().unwrap());
        let got = parse_timestamp_tz(&text)
            .unwrap_or_else(|| panic!("parse_timestamp_tz +hh: {text:?}"));
        prop_assert_eq!(got, PgTimestampTz::from_pg_micros(dt.pg_micros()).unwrap());
    }

    #[test]
    fn timestamp_tz_plus_hhmm_roundtrip(dt in datetime_strategy(), offset in offset_secs_strategy()) {
        let local = fmt_local(dt, offset);
        let (sign, h, m, _s) = offset_parts(offset);
        let text = format!("{} {time}{sign}{h:02}{m:02}", local.date(), time = local.time().unwrap());
        let got = parse_timestamp_tz(&text)
            .unwrap_or_else(|| panic!("parse_timestamp_tz +hhmm: {text:?}"));
        prop_assert_eq!(got, PgTimestampTz::from_pg_micros(dt.pg_micros()).unwrap());
    }

    #[test]
    fn timestamp_tz_plus_hh_colon_mm_roundtrip(dt in datetime_strategy(), offset in offset_secs_strategy()) {
        let local = fmt_local(dt, offset);
        let (sign, h, m, _s) = offset_parts(offset);
        let text = format!("{} {time}{sign}{h:02}:{m:02}", local.date(), time = local.time().unwrap());
        let got = parse_timestamp_tz(&text)
            .unwrap_or_else(|| panic!("parse_timestamp_tz +hh:mm: {text:?}"));
        prop_assert_eq!(got, PgTimestampTz::from_pg_micros(dt.pg_micros()).unwrap());
    }

    #[test]
    fn timestamp_tz_plus_hh_colon_mm_colon_ss_roundtrip(dt in datetime_strategy(), offset in second_offset_secs_strategy()) {
        let local = fmt_local(dt, offset);
        let (sign, h, m, s) = offset_parts(offset);
        let text = format!("{} {time}{sign}{h:02}:{m:02}:{s:02}", local.date(), time = local.time().unwrap());
        let got = parse_timestamp_tz(&text)
            .unwrap_or_else(|| panic!("parse_timestamp_tz +hh:mm:ss: {text:?}"));
        prop_assert_eq!(got, PgTimestampTz::from_pg_micros(dt.pg_micros()).unwrap());
    }

    #[test]
    fn date_iso_roundtrip(d in date_strategy()) {
        let text = d.to_string();
        let got = parse_date(&text)
            .unwrap_or_else(|| panic!("parse_date: {text:?}"));
        prop_assert_eq!(got, d);
    }

    #[test]
    fn time_iso_roundtrip(t in time_strategy()) {
        let text = t.to_string();
        let got = parse_time(&text)
            .unwrap_or_else(|| panic!("parse_time: {text:?}"));
        prop_assert_eq!(got, t);
    }

    #[test]
    fn date_pg_full_range_roundtrip(d in full_range_date_strategy()) {
        let text = d.to_string();
        prop_assert_eq!(parse_date(&text), Some(d), "{}", text);
    }

    #[test]
    fn timestamp_pg_full_range_roundtrip(ts in full_range_timestamp_strategy()) {
        // The ` BC` suffix trails the whole value, so the spelling is the value's own print.
        let text = ts.to_string();
        prop_assert_eq!(parse_timestamp(&text), Some(ts), "{}", text);
    }

    #[test]
    fn timestamp_tz_pg_full_range_roundtrip(
        ts in full_range_timestamp_strategy(),
        offset in second_offset_secs_strategy(),
    ) {
        let Some(local) =
            PgTimestamp::from_pg_micros(ts.pg_micros() + i64::from(offset) * 1_000_000)
        else {
            return Ok(());
        };
        let (sign, h, m, s) = offset_parts(offset);
        let zone = format!("{sign}{h:02}:{m:02}:{s:02}");
        // The offset sits before the ` BC` suffix, which trails the whole value.
        let local_text = local.to_string();
        let text = if let Some(stripped) = local_text.strip_suffix(" BC") {
            format!("{stripped}{zone} BC")
        } else {
            format!("{local_text}{zone}")
        };
        prop_assert_eq!(
            parse_timestamp_tz(&text),
            Some(PgTimestampTz::from_pg_micros(ts.pg_micros()).unwrap()),
            "{}",
            text
        );
    }
}

#[test]
fn timestamp_corpus_idempotence() {
    let corpus = [
        "2026-01-01 00:00:00",
        "2026-01-01T00:00:00",
        "2026-01-01 00:00:00.5",
        "2026-01-01T00:00:00.5",
        "2024-01-15 10:30:45",
        "2024-01-15 10:30:45.123456",
        "2023-12-31 23:59:59.999999",
    ];
    for &s in &corpus {
        let v = parse_timestamp(s).unwrap_or_else(|| panic!("first parse: {s:?}"));
        let text = v.to_string();
        let w = parse_timestamp(&text).unwrap_or_else(|| panic!("second parse: {text:?}"));
        assert_eq!(v, w, "idempotence: {s:?}");
    }
}

#[test]
fn timestamp_tz_corpus_idempotence() {
    let corpus = [
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
    ];
    for &s in &corpus {
        let v = parse_timestamp_tz(s).unwrap_or_else(|| panic!("first parse: {s:?}"));
        let text = v.to_string();
        let w = parse_timestamp_tz(&text).unwrap_or_else(|| panic!("second parse: {text:?}"));
        assert_eq!(v, w, "idempotence: {s:?}");
    }
}

#[test]
fn date_corpus_idempotence() {
    let corpus = ["2026-01-01", "2024-01-15", "2000-02-29", "1970-01-01"];
    for &s in &corpus {
        let v = parse_date(s).unwrap_or_else(|| panic!("first parse: {s:?}"));
        let text = v.to_string();
        let w = parse_date(&text).unwrap_or_else(|| panic!("second parse: {text:?}"));
        assert_eq!(v, w, "idempotence: {s:?}");
    }
}

#[test]
fn time_corpus_idempotence() {
    let corpus = ["00:00:00", "12:34:56.789", "10:30:45", "23:59:59"];
    for &s in &corpus {
        let v = parse_time(s).unwrap_or_else(|| panic!("first parse: {s:?}"));
        let text = v.to_string();
        let w = parse_time(&text).unwrap_or_else(|| panic!("second parse: {text:?}"));
        assert_eq!(v, w, "idempotence: {s:?}");
    }
}

#[test]
fn bool_corpus_idempotence() {
    let corpus = [("t", true), ("f", false), ("1", true), ("0", false)];
    for &(s, expected) in &corpus {
        let v = parse_bool(s).unwrap_or_else(|| panic!("first parse: {s:?}"));
        assert_eq!(v, expected);
        let canonical = if v { "t" } else { "f" };
        let w = parse_bool(canonical).unwrap_or_else(|| panic!("second parse: {canonical:?}"));
        assert_eq!(v, w, "idempotence: {s:?}");
    }
}

#[test]
fn bytea_corpus_idempotence() {
    let corpus = [
        (r"\x", vec![]),
        (r"\x00", vec![0u8]),
        (r"\x0001ff", vec![0u8, 1, 255]),
        (r"\xABCDEF", vec![0xabu8, 0xcd, 0xef]),
    ];
    for (s, bytes) in &corpus {
        let v = parse_pg_bytea_hex(s).unwrap_or_else(|| panic!("first parse: {s:?}"));
        assert_eq!(&v, bytes);
        let mut canonical = String::with_capacity(2 + v.len() * 2);
        canonical.push_str("\\x");
        for b in &v {
            let _ = write!(&mut canonical, "{b:02x}");
        }
        let w =
            parse_pg_bytea_hex(&canonical).unwrap_or_else(|| panic!("second parse: {canonical:?}"));
        assert_eq!(v, w, "idempotence: {s:?}");
    }
}

#[test]
fn i64_corpus_idempotence() {
    let corpus = [
        "0",
        "-1",
        "+1",
        "9223372036854775807",
        "-9223372036854775808",
    ];
    for &s in &corpus {
        let v = parse_i64(s).unwrap_or_else(|| panic!("first parse: {s:?}"));
        let text = format!("{v}");
        let w = parse_i64(&text).unwrap_or_else(|| panic!("second parse: {text:?}"));
        assert_eq!(v, w, "idempotence: {s:?}");
    }
}

#[test]
fn f64_corpus_idempotence() {
    // Rust formats `Infinity` as `inf`, which `parse_f64` accepts.
    let corpus = [
        "0",
        "-1.5",
        "1e20",
        "3.141592653589793",
        "Infinity",
        "-Infinity",
    ];
    for &s in &corpus {
        let v = parse_f64(s).unwrap_or_else(|| panic!("first parse: {s:?}"));
        let text = format!("{v}");
        let w = parse_f64(&text).unwrap_or_else(|| panic!("second parse: {text:?}"));
        assert_eq!(v, w, "idempotence: {s:?}");
    }
}

#[cfg(feature = "decimal")]
#[test]
fn decimal_corpus_idempotence() {
    use sql_values::parse_decimal;

    let corpus = ["0", "-1.5", "+1", "1e20", "12345678901234567890.123456789"];
    for &s in &corpus {
        let v = parse_decimal(s).unwrap_or_else(|| panic!("first parse: {s:?}"));
        let text = v.to_string();
        let w = parse_decimal(&text).unwrap_or_else(|| panic!("second parse: {text:?}"));
        assert_eq!(v, w, "idempotence: {s:?}");
    }
}

fn interval_strategy() -> impl Strategy<Value = PgInterval> {
    let months = prop_oneof![any::<i32>(), -30i32..30];
    let days = prop_oneof![any::<i32>(), -40i32..40];
    let microseconds = prop_oneof![any::<i64>(), -200_000_000_000i64..200_000_000_000];
    (months, days, microseconds).prop_map(|(months, days, microseconds)| PgInterval {
        months,
        days,
        microseconds,
    })
}

proptest! {
    #[test]
    fn interval_postgres_text_roundtrip(interval in interval_strategy()) {
        // PostgreSQL cannot read back the clock it prints for this count.
        prop_assume!(!interval.is_finite() || interval.microseconds != i64::MIN);
        let text = interval.to_string();
        let got = parse_interval(&text)
            .unwrap_or_else(|error| panic!("parse_interval: {text:?}: {error}"));
        prop_assert_eq!(
            (got.months, got.days, got.microseconds),
            (interval.months, interval.days, interval.microseconds),
            "{}",
            text
        );
    }

    #[test]
    fn interval_month_as_thirty_days_is_equal_and_hashes_equal(interval in interval_strategy()) {
        let Some(days) = interval.days.checked_add(30) else { return Ok(()) };
        let Some(months) = interval.months.checked_sub(1) else { return Ok(()) };
        let shifted = PgInterval { months, days, ..interval };
        let hasher = std::hash::RandomState::new();
        prop_assert_eq!(interval, shifted);
        prop_assert_eq!(hasher.hash_one(interval), hasher.hash_one(shifted));
    }

    #[test]
    fn interval_day_as_twenty_four_hours_is_equal_and_hashes_equal(interval in interval_strategy()) {
        let Some(microseconds) = interval.microseconds.checked_add(86_400_000_000) else { return Ok(()) };
        let Some(days) = interval.days.checked_sub(1) else { return Ok(()) };
        let shifted = PgInterval { days, microseconds, ..interval };
        let hasher = std::hash::RandomState::new();
        prop_assert_eq!(interval, shifted);
        prop_assert_eq!(hasher.hash_one(interval), hasher.hash_one(shifted));
    }
}
