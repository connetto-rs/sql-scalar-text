#[path = "support/containers.rs"]
mod containers;
#[path = "support/exit_slot.rs"]
mod exit_slot;
#[path = "support/fractional_matrix.rs"]
mod fractional_matrix;
#[path = "support/postgres.rs"]
mod postgres;

use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use diesel::prelude::*;
use diesel::sql_types::Text;
use sql_scalar_text::{
    parse_bool, parse_date, parse_f64, parse_i64, parse_pg_bytea_hex, parse_time, parse_timestamp,
    parse_timestamp_tz,
};

/// One container per test binary, removed when the binary exits; a plain
/// `static` would keep it running forever, see `exit_slot`.
static PG: postgres::Slot = exit_slot::ExitSlot::new();

fn pg_conn() -> PgConnection {
    postgres::connect(&PG, "16.15-alpine")
}

// Raw SQL: vendor cast syntax cannot be expressed in the Diesel DSL.
fn pg_text(conn: &mut PgConnection, expr: &str) -> String {
    diesel::select(diesel::dsl::sql::<Text>(expr))
        .get_result(conn)
        .unwrap_or_else(|e| panic!("pg_text({expr}): {e}"))
}

fn set_timezone(conn: &mut PgConnection, zone: &str) {
    // Raw SQL: SET timezone is a session command with no DSL equivalent.
    diesel::sql_query(format!("SET timezone = '{zone}'"))
        .execute(conn)
        .unwrap_or_else(|e| panic!("SET timezone={zone}: {e}"));
}

#[test]
fn pg_timestamp_fractional_precision_matrix() {
    let mut conn = pg_conn();

    let cases: &[(&str, u32)] = &[
        ("'2024-01-15 10:30:45'::timestamp(0)::text", 0),
        ("'2024-01-15 10:30:45.1'::timestamp(1)::text", 100_000_000),
        ("'2024-01-15 10:30:45.12'::timestamp(2)::text", 120_000_000),
        ("'2024-01-15 10:30:45.123'::timestamp(3)::text", 123_000_000),
        (
            "'2024-01-15 10:30:45.1234'::timestamp(4)::text",
            123_400_000,
        ),
        (
            "'2024-01-15 10:30:45.12345'::timestamp(5)::text",
            123_450_000,
        ),
        (
            "'2024-01-15 10:30:45.123456'::timestamp(6)::text",
            123_456_000,
        ),
    ];

    fractional_matrix::assert_fractional_seconds(cases, |expr| pg_text(&mut conn, expr));
}

#[test]
fn pg_timestamptz_offset_forms() {
    let mut conn = pg_conn();

    let utc_instant: DateTime<Utc> = "2024-06-15T12:00:00Z".parse().unwrap();
    let pg_literal = "'2024-06-15 12:00:00+00'::timestamptz::text";

    // DST-free zones covering +00, +05:00, +05:30, and -08:00 offset forms.
    let zones: &[&str] = &["UTC", "Etc/GMT-5", "Asia/Kolkata", "Etc/GMT+8"];

    for &zone in zones {
        set_timezone(&mut conn, zone);
        let text = pg_text(&mut conn, pg_literal);
        let got = parse_timestamp_tz(&text)
            .unwrap_or_else(|| panic!("parse_timestamp_tz: {text:?} (zone={zone})"));
        assert_eq!(got, utc_instant, "zone={zone} text={text:?}");
    }
}

#[test]
fn pg_timestamptz_local_mean_time_offsets() {
    let mut conn = pg_conn();

    // Before a zone adopted standard time PostgreSQL prints its local mean
    // time, whose offset carries seconds.
    let cases: &[(&str, &str, &str)] = &[
        ("Europe/Brussels", "1850-01-01T00:00:00Z", "+00:17:30"),
        ("Europe/Dublin", "1870-01-01T00:00:00.544453Z", "-00:25:21"),
        ("America/New_York", "1850-01-01T00:00:00Z", "-04:56:02"),
    ];

    for &(zone, instant, offset) in cases {
        set_timezone(&mut conn, zone);
        let text = pg_text(&mut conn, &format!("'{instant}'::timestamptz::text"));
        assert!(text.ends_with(offset), "zone={zone} text={text:?}");
        let expected: DateTime<Utc> = instant.parse().unwrap();
        let got = parse_timestamp_tz(&text)
            .unwrap_or_else(|| panic!("parse_timestamp_tz: {text:?} (zone={zone})"));
        assert_eq!(got, expected, "zone={zone} text={text:?}");
    }
}

#[test]
fn pg_years_past_9999_and_before_1_ad() {
    let mut conn = pg_conn();
    set_timezone(&mut conn, "UTC");
    let ymd = |y, m, d| NaiveDate::from_ymd_opt(y, m, d).unwrap();

    let tz_cases = [
        (
            "'9999-12-31 23:00:00-01'::timestamptz",
            ymd(10000, 1, 1).and_hms_opt(0, 0, 0).unwrap(),
        ),
        (
            "'0001-01-01 00:30:00+01'::timestamptz",
            ymd(0, 12, 31).and_hms_opt(23, 30, 0).unwrap(),
        ),
    ];
    for (expr, expected) in tz_cases {
        let text = pg_text(&mut conn, &format!("{expr}::text"));
        assert_eq!(
            parse_timestamp_tz(&text),
            Some(expected.and_utc()),
            "{text:?}"
        );
    }

    let text = pg_text(&mut conn, "'0044-03-15 12:00:00 BC'::timestamp::text");
    let expected = ymd(-43, 3, 15).and_hms_opt(12, 0, 0).unwrap();
    assert_eq!(parse_timestamp(&text), Some(expected), "{text:?}");

    for (expr, expected) in [
        ("'123456-01-01'::date", ymd(123_456, 1, 1)),
        ("'0001-01-01 BC'::date", ymd(0, 1, 1)),
    ] {
        let text = pg_text(&mut conn, &format!("{expr}::text"));
        assert_eq!(parse_date(&text), Some(expected), "{text:?}");
    }
}

#[test]
fn pg_scalar_types() {
    let mut conn = pg_conn();

    let date_text = pg_text(&mut conn, "'2024-03-01'::date::text");
    let got_date = parse_date(&date_text).unwrap_or_else(|| panic!("parse_date: {date_text:?}"));
    assert_eq!(got_date, NaiveDate::from_ymd_opt(2024, 3, 1).unwrap());

    let time_text = pg_text(&mut conn, "'14:05:06.789012'::time::text");
    let got_time = parse_time(&time_text).unwrap_or_else(|| panic!("parse_time: {time_text:?}"));
    assert_eq!(
        got_time,
        NaiveTime::from_hms_micro_opt(14, 5, 6, 789_012).unwrap(),
        "time text={time_text:?}"
    );

    // PostgreSQL's Boolean text cast differs from its wire spelling.
    let t_text = pg_text(&mut conn, "CASE WHEN true THEN 't' ELSE 'f' END");
    let f_text = pg_text(&mut conn, "CASE WHEN false THEN 't' ELSE 'f' END");
    assert_eq!(parse_bool(&t_text), Some(true), "bool true: {t_text:?}");
    assert_eq!(parse_bool(&f_text), Some(false), "bool false: {f_text:?}");

    let bytea_text = pg_text(&mut conn, r"'\xDEADBEEF'::bytea::text");
    let got_bytes = parse_pg_bytea_hex(&bytea_text)
        .unwrap_or_else(|| panic!("parse_pg_bytea_hex: {bytea_text:?}"));
    assert_eq!(got_bytes, vec![0xde, 0xad, 0xbe, 0xef]);

    let empty_text = pg_text(&mut conn, r"'\x'::bytea::text");
    assert_eq!(parse_pg_bytea_hex(&empty_text), Some(vec![]));
}

#[cfg(feature = "decimal")]
#[test]
fn pg_decimal_beyond_f64_precision() {
    use bigdecimal::BigDecimal;
    use core::str::FromStr;
    use sql_scalar_text::parse_decimal;

    let mut conn = pg_conn();
    let expr = "'12345678901234567890.123456789'::numeric::text";
    let text = pg_text(&mut conn, expr);
    let got = parse_decimal(&text).unwrap_or_else(|| panic!("parse_decimal: {text:?}"));
    let expected = BigDecimal::from_str("12345678901234567890.123456789").unwrap();
    assert_eq!(got, expected, "decimal text={text:?}");
}

#[test]
fn pg_integer_and_float_text() {
    let mut conn = pg_conn();

    let i64_cases: &[(&str, i64)] = &[
        ("'0'::bigint::text", 0),
        ("'42'::bigint::text", 42),
        ("'-42'::bigint::text", -42),
        ("'9223372036854775807'::bigint::text", i64::MAX),
        ("'-9223372036854775808'::bigint::text", i64::MIN),
    ];
    for &(expr, expected) in i64_cases {
        let text = pg_text(&mut conn, expr);
        let got = parse_i64(&text).unwrap_or_else(|| panic!("parse_i64: {text:?} (expr={expr})"));
        assert_eq!(got, expected, "expr={expr} text={text:?}");
    }

    let f64_finite_cases: &[(&str, f64)] = &[
        ("'0'::float8::text", 0.0),
        ("'2.5'::float8::text", 2.5_f64),
        ("'-1.5'::float8::text", -1.5_f64),
        ("'1e20'::float8::text", 1e20_f64),
    ];
    for &(expr, expected) in f64_finite_cases {
        let text = pg_text(&mut conn, expr);
        let got = parse_f64(&text).unwrap_or_else(|| panic!("parse_f64: {text:?} (expr={expr})"));
        assert_eq!(got, expected, "expr={expr} text={text:?}");
    }

    let inf_text = pg_text(&mut conn, "'Infinity'::float8::text");
    let got_inf =
        parse_f64(&inf_text).unwrap_or_else(|| panic!("parse_f64 Infinity: {inf_text:?}"));
    assert!(
        got_inf.is_infinite() && got_inf.is_sign_positive(),
        "text={inf_text:?}"
    );

    let neg_inf_text = pg_text(&mut conn, "'-Infinity'::float8::text");
    let got_neg =
        parse_f64(&neg_inf_text).unwrap_or_else(|| panic!("parse_f64 -Infinity: {neg_inf_text:?}"));
    assert!(
        got_neg.is_infinite() && got_neg.is_sign_negative(),
        "text={neg_inf_text:?}"
    );

    let nan_text = pg_text(&mut conn, "'NaN'::float8::text");
    let got_nan = parse_f64(&nan_text).unwrap_or_else(|| panic!("parse_f64 NaN: {nan_text:?}"));
    assert!(got_nan.is_nan(), "text={nan_text:?}");
}
