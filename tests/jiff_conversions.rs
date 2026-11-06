//! Conversions to and from jiff's types over the values both hold.
#![cfg(feature = "jiff")]

use jiff::{
    Timestamp,
    civil::{Date, DateTime, Time},
};
use sql_values::{
    DateTimeError, InputContext, PgDate, PgTime, PgTimestamp, PgTimestampTz, parse_date,
    parse_time, parse_timestamp,
};

#[test]
fn dates_convert_within_jiff_years() {
    for text in ["2024-02-29", "9999-12-31", "4714-11-24 BC", "0001-01-01 BC"] {
        let date = parse_date(text).unwrap();
        let civil = Date::try_from(date).unwrap_or_else(|error| panic!("{text}: {error}"));
        assert_eq!(PgDate::try_from(civil), Ok(date), "{text}");
    }
    assert_eq!(
        Date::try_from(parse_date("2024-02-29").unwrap()),
        Ok(jiff::civil::date(2024, 2, 29))
    );
    assert_eq!(
        Date::try_from(parse_date("0001-01-01 BC").unwrap()),
        Ok(jiff::civil::date(0, 1, 1))
    );
    for text in ["10000-01-01", "infinity", "-infinity"] {
        assert_eq!(
            Date::try_from(parse_date(text).unwrap()),
            Err(DateTimeError::OutsideJiffRange)
        );
    }
    assert_eq!(
        PgDate::try_from(jiff::civil::date(-4713, 11, 23)),
        Err(DateTimeError::DateOutOfRange)
    );
}

#[test]
fn times_convert_rounding_nanoseconds_half_to_even() {
    let time = parse_time("12:34:56.789012").unwrap();
    assert_eq!(
        Time::try_from(time),
        Ok(jiff::civil::time(12, 34, 56, 789_012_000))
    );
    assert_eq!(
        Time::try_from(PgTime::from_micros(86_400_000_000).unwrap()),
        Err(DateTimeError::FieldValueOutOfRange)
    );
    let read = |nanos| PgTime::from(jiff::civil::time(0, 0, 0, nanos)).to_string();
    assert_eq!(read(1_500), "00:00:00.000002");
    assert_eq!(read(2_500), "00:00:00.000002");
    assert_eq!(read(2_501), "00:00:00.000003");
    assert_eq!(read(499), "00:00:00");
    assert_eq!(
        PgTime::from(jiff::civil::time(23, 59, 59, 999_999_500)).to_string(),
        "24:00:00"
    );
}

#[test]
fn timestamps_convert_within_jiff_years() {
    for text in [
        "2026-03-29 02:30:00.000001",
        "9999-12-31 23:59:59.999999",
        "4714-11-24 00:00:00 BC",
    ] {
        let at = parse_timestamp(text).unwrap();
        let civil = DateTime::try_from(at).unwrap_or_else(|error| panic!("{text}: {error}"));
        assert_eq!(PgTimestamp::try_from(civil), Ok(at), "{text}");
    }
    for text in ["10000-01-01 00:00:00", "infinity", "-infinity"] {
        assert_eq!(
            DateTime::try_from(parse_timestamp(text).unwrap()),
            Err(DateTimeError::OutsideJiffRange)
        );
    }
    let late = jiff::civil::date(9999, 12, 31).at(23, 59, 59, 999_999_500);
    assert_eq!(
        PgTimestamp::try_from(late).map(|at| at.to_string()),
        Ok("10000-01-01 00:00:00".to_owned())
    );
    let early = jiff::civil::date(-5000, 1, 1).at(0, 0, 0, 0);
    assert_eq!(
        PgTimestamp::try_from(early),
        Err(DateTimeError::TimestampOutOfRange)
    );
}

#[test]
fn instants_convert_within_jiff_range() {
    let context = InputContext::new();
    for text in [
        "2026-03-29 01:00:00+00",
        "1969-12-31 23:59:59.999999+00",
        "4714-11-24 00:00:00+00 BC",
    ] {
        let at = PgTimestampTz::parse(text, context).unwrap();
        let instant = Timestamp::try_from(at).unwrap_or_else(|error| panic!("{text}: {error}"));
        assert_eq!(PgTimestampTz::try_from(instant), Ok(at), "{text}");
    }
    let epoch = PgTimestampTz::parse("1970-01-01 00:00:00+00", context).unwrap();
    assert_eq!(Timestamp::try_from(epoch), Ok(Timestamp::UNIX_EPOCH));
    for text in ["10000-01-01 00:00:00+00", "infinity", "-infinity"] {
        let at = PgTimestampTz::parse(text, context).unwrap();
        assert_eq!(
            Timestamp::try_from(at),
            Err(DateTimeError::OutsideJiffRange)
        );
    }
    let before_epoch = Timestamp::from_nanosecond(-1_500).unwrap();
    assert_eq!(
        PgTimestampTz::try_from(before_epoch).map(|at| at.to_string()),
        Ok("1969-12-31 23:59:59.999998+00".to_owned())
    );
    assert_eq!(
        PgTimestampTz::try_from(Timestamp::MIN),
        Err(DateTimeError::TimestampOutOfRange)
    );
}
