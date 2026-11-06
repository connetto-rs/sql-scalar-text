#[path = "support/containers.rs"]
mod containers;
#[path = "support/exit_slot.rs"]
mod exit_slot;
#[path = "support/fractional_matrix.rs"]
mod fractional_matrix;
#[path = "support/postgres.rs"]
mod postgres;

use diesel::{dsl::sql, prelude::*, result::Error as DieselError, sql_types::Text};
use sql_values::{
    DateTimeError, PgDate, PgTime, PgTimeTz, parse_bool, parse_date, parse_f64, parse_i64,
    parse_pg_bytea_hex, parse_time, parse_timestamp, parse_timestamp_tz,
};

/// One container per test binary, removed when the binary exits; a plain
/// `static` would keep it running forever, see `exit_slot`.
static PG: postgres::Slot = exit_slot::ExitSlot::new();

fn pg_conn() -> PgConnection {
    postgres::connect(&PG, "16.15-alpine")
}

static PG18: postgres::Slot = exit_slot::ExitSlot::new();

/// A PostgreSQL 18.6 session under `TimeZone` `UTC`.
fn pg18_conn() -> PgConnection {
    let mut conn = postgres::connect(&PG18, "18.6");
    set_timezone(&mut conn, "UTC");
    conn
}

/// What `expression` prints through its type's output function on PostgreSQL 18.6, or the
/// message it raises without the quoted input PostgreSQL appends.
fn pg18_answer(conn: &mut PgConnection, expression: &str) -> Result<String, String> {
    // Raw SQL with the literal inline, so that it reaches the type's input function.
    match diesel::select(sql::<Text>(&format!("format('%s', {expression})"))).get_result(conn) {
        Ok(text) => Ok(text),
        Err(DieselError::DatabaseError(_, info)) => Err(info
            .message()
            .split_once(": \"")
            .map_or(info.message(), |(stem, _)| stem)
            .to_owned()),
        Err(other) => panic!("{expression}: {other}"),
    }
}

/// `text` quoted as an SQL string literal.
fn quoted(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// The crate's answer in PostgreSQL's words, its value printed or its error's message.
fn worded<T: std::fmt::Display, E: std::fmt::Display>(
    result: Result<T, E>,
) -> Result<String, String> {
    result
        .map(|value| value.to_string())
        .map_err(|error| error.to_string())
}

/// Every expression against the crate's answer for it, listing the disagreements.
fn assert_answers(conn: &mut PgConnection, cases: Vec<(String, Result<String, String>)>) {
    let mismatches: Vec<String> = cases
        .into_iter()
        .filter_map(|(expression, got)| {
            let expected = pg18_answer(conn, &expression);
            (got != expected)
                .then(|| format!("{expression}: crate {got:?}, PostgreSQL {expected:?}"))
        })
        .collect();
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

/// Time literals at the clock's bounds, rounding, offsets and refusals.
const TIMES: &[&str] = &[
    "00:00",
    "12:00",
    "12:34:56",
    "12:34:56.789",
    " 12:34:56.789012 ",
    "12:00:00.1234565",
    "12:00:00.1234575",
    "12:00:00.0000005",
    "12:00:00.0000015",
    "23:59:59.9999995",
    "23:59:59.999999",
    "24:00",
    "24:00:00",
    "23:59:60",
    "24:00:00.000001",
    "23:59:60.000001",
    "24:00:01",
    "25:00:00",
    "12:60:00",
    "12:00:61",
    "25:00+16",
    "12:61+16",
    "allballs",
    "ALLBALLS",
    "12:00:00+05",
    "12:00:00-05:30",
    "12:00:00+05:30:15",
    "12:00:00-15:59:59",
    "12:00:00+16",
    "12:00:00+0530",
    "12:00:00+123",
    "12:00:00+0560",
    "12:00:00 +05",
    "12:00Z",
    "12:00 z",
    "24:00:00+00",
];

#[test]
fn pg18_reads_every_time_literal() {
    let mut conn = pg18_conn();
    let mut cases = Vec::new();
    for &text in TIMES {
        cases.push((
            format!("{}::time", quoted(text)),
            worded(PgTime::parse(text)),
        ));
        let timetz = PgTimeTz::parse(text);
        if timetz != Err(DateTimeError::Unsupported) {
            cases.push((format!("{}::timetz", quoted(text)), worded(timetz)));
        }
    }
    assert_answers(&mut conn, cases);
}

/// `timetz_cmp` on every pair of the time literals that read as `timetz`.
#[test]
fn pg18_orders_every_timetz_pair() {
    let mut conn = pg18_conn();
    let values: Vec<(&str, PgTimeTz)> = TIMES
        .iter()
        .filter_map(|&text| Some((text, PgTimeTz::parse(text).ok()?)))
        .collect();
    let mut cases = Vec::new();
    for &(left, left_value) in &values {
        for &(right, right_value) in &values {
            // `Ordering`'s discriminants are -1, 0 and 1, as `timetz_cmp` answers.
            let order = left_value.cmp(&right_value) as i8;
            cases.push((
                format!(
                    "timetz_cmp({}::timetz, {}::timetz)",
                    quoted(left),
                    quoted(right)
                ),
                Ok(order.to_string()),
            ));
        }
    }
    assert_answers(&mut conn, cases);
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

    let utc_instant = "2024-06-15 12:00:00+00";
    let pg_literal = "'2024-06-15 12:00:00+00'::timestamptz::text";

    // DST-free zones covering +00, +05:00, +05:30, and -08:00 offset forms.
    let zones: &[&str] = &["UTC", "Etc/GMT-5", "Asia/Kolkata", "Etc/GMT+8"];

    for &zone in zones {
        set_timezone(&mut conn, zone);
        let text = pg_text(&mut conn, pg_literal);
        let got = parse_timestamp_tz(&text)
            .unwrap_or_else(|| panic!("parse_timestamp_tz: {text:?} (zone={zone})"));
        assert_eq!(got.to_string(), utc_instant, "zone={zone} text={text:?}");
    }
}

#[test]
fn pg_timestamptz_local_mean_time_offsets() {
    let mut conn = pg_conn();

    // Before a zone adopted standard time PostgreSQL prints its local mean
    // time, whose offset carries seconds.
    let cases: &[(&str, &str, &str)] = &[
        ("Europe/Brussels", "1850-01-01 00:00:00+00", "+00:17:30"),
        (
            "Europe/Dublin",
            "1870-01-01 00:00:00.544453+00",
            "-00:25:21",
        ),
        ("America/New_York", "1850-01-01 00:00:00+00", "-04:56:02"),
    ];

    for &(zone, instant, offset) in cases {
        set_timezone(&mut conn, zone);
        let text = pg_text(&mut conn, &format!("'{instant}'::timestamptz::text"));
        assert!(text.ends_with(offset), "zone={zone} text={text:?}");
        let got = parse_timestamp_tz(&text)
            .unwrap_or_else(|| panic!("parse_timestamp_tz: {text:?} (zone={zone})"));
        assert_eq!(got.to_string(), instant, "zone={zone} text={text:?}");
    }
}

#[test]
fn pg_years_past_9999_and_before_1_ad() {
    let mut conn = pg_conn();
    set_timezone(&mut conn, "UTC");

    for (expr, expected) in [
        (
            "'9999-12-31 23:00:00-01'::timestamptz",
            "10000-01-01 00:00:00+00",
        ),
        (
            "'0001-01-01 00:30:00+01'::timestamptz",
            "0001-12-31 23:30:00+00 BC",
        ),
    ] {
        let text = pg_text(&mut conn, &format!("{expr}::text"));
        let got = parse_timestamp_tz(&text).map(|at| at.to_string());
        assert_eq!(got.as_deref(), Some(expected), "{text:?}");
    }

    let text = pg_text(&mut conn, "'0044-03-15 12:00:00 BC'::timestamp::text");
    let got = parse_timestamp(&text).map(|at| at.to_string());
    assert_eq!(got.as_deref(), Some("0044-03-15 12:00:00 BC"), "{text:?}");

    for (expr, expected) in [
        ("'123456-01-01'::date", "123456-01-01"),
        ("'0001-01-01 BC'::date", "0001-01-01 BC"),
    ] {
        let text = pg_text(&mut conn, &format!("{expr}::text"));
        let got = parse_date(&text).map(|date| date.to_string());
        assert_eq!(got.as_deref(), Some(expected), "{text:?}");
    }
}

#[test]
fn pg_scalar_types() {
    let mut conn = pg_conn();

    let date_text = pg_text(&mut conn, "'2024-03-01'::date::text");
    let got_date = parse_date(&date_text).unwrap_or_else(|| panic!("parse_date: {date_text:?}"));
    assert_eq!(got_date, PgDate::from_ymd(2024, 3, 1).unwrap());

    let time_text = pg_text(&mut conn, "'14:05:06.789012'::time::text");
    let got_time = parse_time(&time_text).unwrap_or_else(|| panic!("parse_time: {time_text:?}"));
    assert_eq!(
        got_time,
        PgTime::from_hms_micro(14, 5, 6, 789_012).unwrap(),
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
    use sql_values::parse_decimal;

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
