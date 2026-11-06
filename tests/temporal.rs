use sql_values::{
    DateTimeError, InputContext, PgTime, PgTimeTz, PgTimestampTz, parse_date, parse_time,
    parse_timestamp, parse_timestamp_tz,
};

/// Which reader a corpus entry goes through.
#[derive(Debug, Clone, Copy)]
enum Reader {
    Timestamp,
    TimestampTz,
    Date,
    Time,
}

/// The text PostgreSQL prints for what `reader` reads from `text`, `None` when it refuses it.
fn read(reader: Reader, text: &str) -> Option<String> {
    match reader {
        Reader::Timestamp => parse_timestamp(text).map(|v| v.to_string()),
        Reader::TimestampTz => parse_timestamp_tz(text).map(|v| v.to_string()),
        Reader::Date => parse_date(text).map(|v| v.to_string()),
        Reader::Time => parse_time(text).map(|v| v.to_string()),
    }
}

/// Spellings each reader takes, with the value PostgreSQL prints for it under `TimeZone` `UTC`.
const ACCEPTED: &[(&str, Reader, &str)] = &[
    (
        "2026-01-01 00:00:00+00",
        Reader::TimestampTz,
        "2026-01-01 00:00:00+00",
    ),
    (
        "2026-01-01 00:00:00+00:00",
        Reader::TimestampTz,
        "2026-01-01 00:00:00+00",
    ),
    (
        "2026-01-01 00:00:00+0000",
        Reader::TimestampTz,
        "2026-01-01 00:00:00+00",
    ),
    (
        "2026-01-01 00:00:00Z",
        Reader::TimestampTz,
        "2026-01-01 00:00:00+00",
    ),
    (
        "2026-01-01T00:00:00Z",
        Reader::TimestampTz,
        "2026-01-01 00:00:00+00",
    ),
    (
        "2026-01-01T00:00:00+00:00",
        Reader::TimestampTz,
        "2026-01-01 00:00:00+00",
    ),
    (
        "2025-12-31 22:00:00-02",
        Reader::TimestampTz,
        "2026-01-01 00:00:00+00",
    ),
    (
        "2026-01-01 02:30:00+02:30",
        Reader::TimestampTz,
        "2026-01-01 00:00:00+00",
    ),
    (
        "1900-01-01 00:19:32+00:19:32",
        Reader::TimestampTz,
        "1900-01-01 00:00:00+00",
    ),
    (
        "1920-09-11 04:04:24.544453+01:19:32",
        Reader::TimestampTz,
        "1920-09-11 02:44:52.544453+00",
    ),
    (
        "1869-12-31 23:34:39-00:25:21",
        Reader::TimestampTz,
        "1870-01-01 00:00:00+00",
    ),
    (
        "2026-01-01 00:00:00.5+00",
        Reader::TimestampTz,
        "2026-01-01 00:00:00.5+00",
    ),
    (
        "2026-01-01T00:00:00.5Z",
        Reader::TimestampTz,
        "2026-01-01 00:00:00.5+00",
    ),
    (
        "2026-01-01 00:00:00",
        Reader::Timestamp,
        "2026-01-01 00:00:00",
    ),
    (
        "2026-01-01T00:00:00",
        Reader::Timestamp,
        "2026-01-01 00:00:00",
    ),
    (
        "2026-01-01 00:00:00.5",
        Reader::Timestamp,
        "2026-01-01 00:00:00.5",
    ),
    (
        "2026-01-01T00:00:00.5",
        Reader::Timestamp,
        "2026-01-01 00:00:00.5",
    ),
    ("2026-01-01", Reader::Date, "2026-01-01"),
    ("00:00:00", Reader::Time, "00:00:00"),
    ("12:34:56.789", Reader::Time, "12:34:56.789"),
    (
        "10000-01-01 00:00:00",
        Reader::Timestamp,
        "10000-01-01 00:00:00",
    ),
    (
        "0044-03-15 12:00:00 BC",
        Reader::Timestamp,
        "0044-03-15 12:00:00 BC",
    ),
    (
        "9999-12-31 23:00:00-01",
        Reader::TimestampTz,
        "10000-01-01 00:00:00+00",
    ),
    (
        "10000-01-01 00:00:00+00",
        Reader::TimestampTz,
        "10000-01-01 00:00:00+00",
    ),
    (
        "0001-12-31 23:30:00+00 BC",
        Reader::TimestampTz,
        "0001-12-31 23:30:00+00 BC",
    ),
    ("123456-01-01", Reader::Date, "123456-01-01"),
    ("0001-01-01 BC", Reader::Date, "0001-01-01 BC"),
    ("0044-03-15 BC", Reader::Date, "0044-03-15 BC"),
    ("0000-02-29", Reader::Date, "0001-02-29 BC"),
    (
        "0000-01-01 00:00:00",
        Reader::Timestamp,
        "0001-01-01 00:00:00 BC",
    ),
    (
        "0000-12-31 23:00:00-01",
        Reader::TimestampTz,
        "0001-01-01 00:00:00+00",
    ),
    // PostgreSQL's bounds and infinities, as its output functions print them.
    ("5874897-12-31", Reader::Date, "5874897-12-31"),
    ("4714-11-24 BC", Reader::Date, "4714-11-24 BC"),
    ("infinity", Reader::Date, "infinity"),
    ("-infinity", Reader::Date, "-infinity"),
    (
        "294276-12-31 23:59:59.999999",
        Reader::Timestamp,
        "294276-12-31 23:59:59.999999",
    ),
    (
        "4714-11-24 00:00:00 BC",
        Reader::Timestamp,
        "4714-11-24 00:00:00 BC",
    ),
    ("infinity", Reader::Timestamp, "infinity"),
    ("-infinity", Reader::Timestamp, "-infinity"),
    (
        "294276-12-31 23:59:59.999999+00",
        Reader::TimestampTz,
        "294276-12-31 23:59:59.999999+00",
    ),
    (
        "294277-01-01 00:59:59+01",
        Reader::TimestampTz,
        "294276-12-31 23:59:59+00",
    ),
    (
        "4714-11-24 00:00:00+00 BC",
        Reader::TimestampTz,
        "4714-11-24 00:00:00+00 BC",
    ),
    ("infinity", Reader::TimestampTz, "infinity"),
    ("-infinity", Reader::TimestampTz, "-infinity"),
    // PostgreSQL's 24:00:00, and fractions rounded half to even to the microsecond.
    ("24:00:00", Reader::Time, "24:00:00"),
    (
        "2026-12-31 24:00:00",
        Reader::Timestamp,
        "2027-01-01 00:00:00",
    ),
    ("12:00:00.1234565", Reader::Time, "12:00:00.123456"),
    ("12:00:00.1234575", Reader::Time, "12:00:00.123458"),
    ("23:59:59.9999995", Reader::Time, "24:00:00"),
    (
        "2026-01-01 00:00:00.000000501",
        Reader::Timestamp,
        "2026-01-01 00:00:00.000001",
    ),
];

#[test]
fn parses_temporal_spelling_corpus_to_exact_values() {
    for &(text, reader, expected) in ACCEPTED {
        assert_eq!(
            read(reader, text).as_deref(),
            Some(expected),
            "{reader:?} {text}"
        );
    }
}

#[test]
fn refuses_temporal_negative_corpus() {
    for text in [
        "2026-01-01 00:00:00",
        "2026-01-01",
        "nope",
        "",
        "2026-01-01 00:00:00+24",
        "2026-01-01 00:00:00+24:00",
        "2026-01-01 00:00:00+00:60",
        "2026-01-01 00:00:00+00:99",
        "2026-01-01 00:00:00-24",
        "2026-01-01 00:00:00-00:60",
        "2026-01-01 00:00:00+//",
        "2026-01-01 00:00:00+//:00",
        "2026-01-01 00:00:00+-0",
        "2026-01-01 00:00:00++00",
        "2026-01-01 00:00:00+00:00:60",
        "2026-01-01 00:00:00+00:60:00",
        "2026-01-01 00:00:00+24:00:00",
        "2026-01-01 00:00:00-00:00:99",
        "2026-01-01 00:00:00+00:00-00",
        "2026-01-01 00:00:00+00-00:00",
        "2026-01-01 00:00:00+0000:00",
        "2026-01-01 00:00:00+00:00:0/",
        "2016-12-31 23:59:60+00",
        "2016-12-31T23:59:60Z",
        "2016-12-31T23:59:60.5+00:00",
        "1850-01-01 00:17:60+00:17:30",
        "+10000-01-01 00:00:00+00",
        "-0001-01-01 00:00:00+00",
        "0000-01-01 00:00:00+00 BC",
        "-0001-12-31 00:00:00+00",
        "010000-01-01 00:00:00+00",
        "294277-01-01 00:00:00+00",
        "294276-12-31 23:59:59-01",
        "4714-11-23 23:59:59+00 BC",
        "2026-1-01 00:00:00+00",
        " 2026-01-01 00:00:00+00",
        "2026-01-01 0:00:00+00",
        "2026-01-01  00:00:00+00",
        "2026-01-01 00:00:00.+00",
        "2026-01-01 00:00:00.1234567890+00",
        "2026-01-01 00:00:00+00BC",
        "2026-01-01 00:00:00 BC+00",
        "2026-02-30 00:00:00+00",
        "2026-01-01X00:00:00+00",
        "2026-01-01 00:00:00+05x30",
    ] {
        assert_eq!(parse_timestamp_tz(text), None, "{text}");
    }
    for text in [
        "2026-01-01 00:00:00+00",
        "2026-01-01",
        "nope",
        "",
        "2016-12-31 23:59:60",
        "2016-12-31T23:59:60.5",
        "+10000-01-01 00:00:00",
        "294277-01-01 00:00:00",
        "4714-11-23 23:59:59.999999 BC",
        "2026-01-01 24:00:01",
        "0000-01-01 00:00:00 BC",
        "-0001-12-31 00:00:00",
        "2026-1-2 3:4:5",
        "2026-01-02 03:04:05 ",
        "2026-01-02 03:04:05 AD",
        "2026-02-30 00:00:00",
        "2026-13-01 00:00:00",
        "2026-01-01X00:00:00",
    ] {
        assert_eq!(parse_timestamp(text), None, "{text}");
    }
    for text in [
        "20260101",
        "nope",
        "",
        "+10000-01-01",
        "-0001-01-01",
        "-0001-12-31",
        "00000-01-01",
        "0000-01-01 BC",
        "2026-1-2",
        " 2026-01-02",
        "2026-01-02 ",
        "0001-01-01BC",
        "5874898-01-01",
        "4714-11-23 BC",
        "2026-02-29",
    ] {
        assert_eq!(parse_date(text), None, "{text}");
    }
    for text in [
        "12:34",
        "nope",
        "",
        "23:59:60",
        "23:59:60.5",
        "24:00:00.000001",
        "24:00:01",
        "12:60:00",
        "3:04:05",
        " 03:04:05",
        "03:04:05.",
        "03:04:05.1234567890",
        "03:04:05 BC",
    ] {
        assert_eq!(parse_time(text), None, "{text}");
    }
}

/// Spellings of `time` and `timetz` PostgreSQL reads by the clock or by grammar the readers do
/// not model are declined rather than misread.
#[test]
fn declines_spellings_it_does_not_model() {
    let context = InputContext::new();
    let unsupported = Err(DateTimeError::Unsupported);
    for text in [
        "now",
        "12:00 PM",
        "1:00",
        "12:00 Europe/Rome",
        "2026-01-01 12:00",
        "040506",
        "12:00:00.000000000000000000000000000000000000000000000000000000000",
        "12:00 BC",
    ] {
        assert_eq!(
            PgTime::parse(text, context).map(|_| ()),
            unsupported,
            "{text}"
        );
        assert_eq!(
            PgTimeTz::parse(text, context).map(|_| ()),
            unsupported,
            "{text}"
        );
    }
    // A `timetz` without an offset, or with a zone whose offset changes, takes the offset of
    // the current date.
    assert_eq!(PgTimeTz::parse("12:00", context).map(|_| ()), unsupported);
    assert_eq!(
        PgTimeTz::parse("12:00 Japan", context).map(|_| ()),
        unsupported
    );
    assert_eq!(
        PgTimeTz::parse("12:00 MSK", context).map(|_| ()),
        unsupported
    );
    assert_eq!(
        PgTimeTz::parse("12:00 Factory", context).map(|t| t.to_string()),
        Ok("12:00:00+00".to_owned())
    );
}

/// A `timestamptz` written without an offset needs the session's time zone.
#[test]
fn timestamptz_without_offset_needs_a_time_zone() {
    assert_eq!(
        PgTimestampTz::parse("2026-01-01 12:00", InputContext::new()),
        Err(DateTimeError::NeedsTimeZone)
    );
    assert!(PgTimestampTz::parse("2026-01-01 12:00+00", InputContext::new()).is_ok());
    assert!(PgTimestampTz::parse("epoch", InputContext::new()).is_ok());
}
