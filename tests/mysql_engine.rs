#[path = "support/containers.rs"]
mod containers;
#[path = "support/exit_slot.rs"]
mod exit_slot;
#[path = "support/fractional_matrix.rs"]
mod fractional_matrix;

use chrono::{NaiveDate, NaiveTime};
use diesel::prelude::*;
use diesel::sql_types::Text;
use sql_scalar_text::{parse_bool, parse_date, parse_f64, parse_i64, parse_time, parse_timestamp};
use testcontainers::{
    GenericImage, ImageExt,
    core::{IntoContainerPort, WaitFor},
    runners::SyncRunner,
};

/// One container per test binary, removed when the binary exits; a plain
/// `static` would keep it running forever, see `exit_slot`.
static MYSQL: exit_slot::ExitSlot<testcontainers::Container<GenericImage>> =
    exit_slot::ExitSlot::new();

fn mysql_port() -> u16 {
    MYSQL.with(
        || {
            containers::sweep_abandoned();
            GenericImage::new("mysql", "8.4.11")
                .with_exposed_port(3306.tcp())
                .with_wait_for(WaitFor::message_on_stderr("port: 3306"))
                .with_env_var("MYSQL_ROOT_PASSWORD", "root")
                .with_env_var("MYSQL_DATABASE", "test")
                .with_labels(containers::labels("mysql"))
                .start()
                .expect("start mysql 8.4.11")
        },
        |container| container.get_host_port_ipv4(3306).expect("get mysql port"),
    )
}

fn mysql_conn() -> MysqlConnection {
    let port = mysql_port();
    let url = format!("mysql://root:root@127.0.0.1:{port}/test");
    MysqlConnection::establish(&url).expect("connect to mysql")
}

// Raw SQL: CAST AS CHAR is MySQL-specific syntax with no Diesel DSL equivalent.
fn mysql_text(conn: &mut MysqlConnection, expr: &str) -> String {
    diesel::select(diesel::dsl::sql::<Text>(expr))
        .get_result(conn)
        .unwrap_or_else(|e| panic!("mysql_text({expr}): {e}"))
}

#[test]
fn mysql_timestamp_fractional_precision_matrix() {
    let mut conn = mysql_conn();

    let cases: &[(&str, u32)] = &[
        (
            "CAST(CAST('2024-01-15 10:30:45' AS DATETIME(0)) AS CHAR)",
            0,
        ),
        (
            "CAST(CAST('2024-01-15 10:30:45.1' AS DATETIME(1)) AS CHAR)",
            100_000_000,
        ),
        (
            "CAST(CAST('2024-01-15 10:30:45.12' AS DATETIME(2)) AS CHAR)",
            120_000_000,
        ),
        (
            "CAST(CAST('2024-01-15 10:30:45.123' AS DATETIME(3)) AS CHAR)",
            123_000_000,
        ),
        (
            "CAST(CAST('2024-01-15 10:30:45.1234' AS DATETIME(4)) AS CHAR)",
            123_400_000,
        ),
        (
            "CAST(CAST('2024-01-15 10:30:45.12345' AS DATETIME(5)) AS CHAR)",
            123_450_000,
        ),
        (
            "CAST(CAST('2024-01-15 10:30:45.123456' AS DATETIME(6)) AS CHAR)",
            123_456_000,
        ),
    ];

    fractional_matrix::assert_fractional_seconds(cases, |expr| mysql_text(&mut conn, expr));
}

#[test]
fn mysql_year_zero() {
    let mut conn = mysql_conn();
    let year_zero = NaiveDate::from_ymd_opt(0, 1, 1).unwrap();

    let text = mysql_text(&mut conn, "CAST(CAST('0000-01-01' AS DATE) AS CHAR)");
    assert_eq!(parse_date(&text), Some(year_zero), "{text:?}");

    let text = mysql_text(
        &mut conn,
        "CAST(CAST('0000-01-01 12:00:00' AS DATETIME) AS CHAR)",
    );
    let expected = year_zero.and_hms_opt(12, 0, 0).unwrap();
    assert_eq!(parse_timestamp(&text), Some(expected), "{text:?}");
}

#[test]
fn mysql_date_and_time_cast_as_char() {
    let mut conn = mysql_conn();

    let date_text = mysql_text(&mut conn, "CAST(DATE'2024-03-15' AS CHAR)");
    let got_date = parse_date(&date_text).unwrap_or_else(|| panic!("parse_date: {date_text:?}"));
    assert_eq!(got_date, NaiveDate::from_ymd_opt(2024, 3, 15).unwrap());

    let time_text = mysql_text(&mut conn, "CAST(TIME'14:05:06' AS CHAR)");
    let got_time = parse_time(&time_text).unwrap_or_else(|| panic!("parse_time: {time_text:?}"));
    assert_eq!(got_time, NaiveTime::from_hms_opt(14, 5, 6).unwrap());

    let frac_time_text = mysql_text(
        &mut conn,
        "CAST(CAST('14:05:06.123456' AS TIME(6)) AS CHAR)",
    );
    let got_frac = parse_time(&frac_time_text)
        .unwrap_or_else(|| panic!("parse_time fractional: {frac_time_text:?}"));
    assert_eq!(
        got_frac,
        NaiveTime::from_hms_micro_opt(14, 5, 6, 123_456).unwrap(),
        "frac time text={frac_time_text:?}"
    );
}

#[test]
fn mysql_bool_cast_as_char() {
    let mut conn = mysql_conn();

    let t_text = mysql_text(&mut conn, "CAST(TRUE AS CHAR)");
    let f_text = mysql_text(&mut conn, "CAST(FALSE AS CHAR)");
    assert_eq!(parse_bool(&t_text), Some(true), "TRUE: {t_text:?}");
    assert_eq!(parse_bool(&f_text), Some(false), "FALSE: {f_text:?}");
}

#[cfg(feature = "decimal")]
#[test]
fn mysql_decimal_beyond_f64_precision() {
    use bigdecimal::BigDecimal;
    use core::str::FromStr;
    use sql_scalar_text::parse_decimal;

    let mut conn = mysql_conn();

    let expr = "CAST(12345678901234567890.123456789 AS CHAR)";
    let text = mysql_text(&mut conn, expr);
    let got = parse_decimal(&text).unwrap_or_else(|| panic!("parse_decimal: {text:?}"));
    let expected = BigDecimal::from_str("12345678901234567890.123456789").unwrap();
    assert_eq!(got, expected, "decimal text={text:?}");
}

#[test]
fn mysql_integer_and_float_text() {
    let mut conn = mysql_conn();

    let i64_cases: &[(&str, i64)] = &[
        ("CAST(0 AS CHAR)", 0),
        ("CAST(42 AS CHAR)", 42),
        ("CAST(-42 AS CHAR)", -42),
        ("CAST(9223372036854775807 AS CHAR)", i64::MAX),
        (
            "CAST(CAST('-9223372036854775808' AS SIGNED) AS CHAR)",
            i64::MIN,
        ),
    ];
    for &(expr, expected) in i64_cases {
        let text = mysql_text(&mut conn, expr);
        let got = parse_i64(&text).unwrap_or_else(|| panic!("parse_i64: {text:?} (expr={expr})"));
        assert_eq!(got, expected, "expr={expr} text={text:?}");
    }

    let f64_cases: &[(&str, f64)] = &[
        ("CAST(CAST(0.0 AS DOUBLE) AS CHAR)", 0.0),
        ("CAST(CAST(1.5 AS DOUBLE) AS CHAR)", 1.5),
        ("CAST(CAST(-2.5 AS DOUBLE) AS CHAR)", -2.5),
    ];
    for &(expr, expected) in f64_cases {
        let text = mysql_text(&mut conn, expr);
        let got = parse_f64(&text).unwrap_or_else(|| panic!("parse_f64: {text:?} (expr={expr})"));
        assert_eq!(got, expected, "expr={expr} text={text:?}");
    }
}
