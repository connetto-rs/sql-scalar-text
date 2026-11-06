//! The date and time types' bounds, conversions from what PostgreSQL stores, and order.

use std::collections::HashSet;

use sql_values::{DateTimeError, PgDate, PgTime, PgTimeTz, PgTimeZone, PgTimestamp, PgTimestampTz};

/// 4714-11-24 BC and 5874897-12-31, in days from 2000-01-01.
const FIRST_DAY: i32 = -2_451_545;
const LAST_DAY: i32 = 2_145_031_948;
/// 4714-11-24 00:00:00 BC and 294276-12-31 23:59:59.999999, in microseconds from 2000-01-01.
const FIRST_MICRO: i64 = -211_813_488_000_000_000;
const LAST_MICRO: i64 = 9_223_371_331_199_999_999;

#[test]
fn dates_hold_postgresql_range_and_infinities() {
    let first = PgDate::from_ymd(-4713, 11, 24).expect("4714-11-24 BC");
    let last = PgDate::from_ymd(5_874_897, 12, 31).expect("5874897-12-31");
    assert_eq!((first.pg_days(), last.pg_days()), (FIRST_DAY, LAST_DAY));
    assert_eq!(first.to_string(), "4714-11-24 BC");
    assert_eq!(last.to_string(), "5874897-12-31");
    assert_eq!(first.ymd(), Some((-4713, 11, 24)));
    for (year, month, day) in [
        (-4713, 11, 23),
        (-4714, 12, 31),
        (5_874_898, 1, 1),
        (5_874_899, 1, 1),
        (2026, 2, 29),
        (2026, 13, 1),
        (2026, 1, 0),
    ] {
        assert_eq!(
            PgDate::from_ymd(year, month, day),
            None,
            "{year}-{month}-{day}"
        );
    }
    assert_eq!(
        PgDate::from_ymd(2024, 2, 29)
            .map(|d| d.to_string())
            .as_deref(),
        Some("2024-02-29")
    );
    assert_eq!(
        PgDate::from_ymd(0, 1, 1).map(|d| d.to_string()).as_deref(),
        Some("0001-01-01 BC")
    );

    assert_eq!(PgDate::from_pg_days(FIRST_DAY - 1), None);
    assert_eq!(PgDate::from_pg_days(LAST_DAY + 1), None);
    assert_eq!(PgDate::from_pg_days(i32::MAX), Some(PgDate::INFINITY));
    assert_eq!(PgDate::try_from(i32::MIN), Ok(PgDate::NEG_INFINITY));
    assert_eq!(
        PgDate::try_from(LAST_DAY + 1),
        Err(DateTimeError::DateOutOfRange)
    );
    assert_eq!(i32::from(last), LAST_DAY);
    assert!(PgDate::NEG_INFINITY < first && last < PgDate::INFINITY);
    assert!(!PgDate::INFINITY.is_finite() && last.is_finite());
    assert_eq!(PgDate::INFINITY.ymd(), None);
    assert_eq!(PgDate::NEG_INFINITY.to_string(), "-infinity");
}

#[test]
fn times_hold_midnight_to_twenty_four() {
    let end = PgTime::from_hms_micro(24, 0, 0, 0).expect("24:00:00");
    assert_eq!(end.micros(), 86_400_000_000);
    assert_eq!(end.to_string(), "24:00:00");
    assert_eq!(end.hms_micro(), (24, 0, 0, 0));
    assert_eq!(PgTime::MIDNIGHT.to_string(), "00:00:00");
    assert_eq!(PgTime::from_hms_micro(24, 0, 0, 1), None);
    assert_eq!(PgTime::from_hms_micro(25, 0, 0, 0), None);
    assert_eq!(PgTime::from_hms_micro(23, 60, 0, 0), None);
    assert_eq!(PgTime::from_hms_micro(23, 0, 60, 0), None);
    assert_eq!(PgTime::from_hms_micro(23, 0, 0, 1_000_000), None);
    assert_eq!(PgTime::from_micros(-1), None);
    assert_eq!(PgTime::from_micros(86_400_000_001), None);
    assert_eq!(
        PgTime::try_from(-1),
        Err(DateTimeError::FieldValueOutOfRange)
    );
    let late = PgTime::try_from(86_399_999_999).expect("23:59:59.999999");
    assert_eq!(late.to_string(), "23:59:59.999999");
    assert_eq!(i64::from(late), 86_399_999_999);
    assert!(late < end);
}

#[test]
fn timetz_orders_by_instant_then_offset() {
    let at = |hour, offset| PgTimeTz::new(PgTime::from_hms_micro(hour, 0, 0, 0).unwrap(), offset);
    let rome = at(12, 3_600).expect("12:00+01");
    let london = at(11, 0).expect("11:00+00");
    let new_york = at(6, -18_000).expect("06:00-05");
    assert_eq!(rome.to_string(), "12:00:00+01");
    assert_eq!(new_york.to_string(), "06:00:00-05");
    assert_eq!(
        (rome.time().micros(), rome.offset()),
        (43_200_000_000, 3_600)
    );
    // One instant, ordered by PostgreSQL's zone, seconds west of UTC.
    assert!(rome < london && london < new_york);
    assert_ne!(rome, london);
    assert!(at(12, 0).unwrap() > rome);
    assert_eq!(
        at(0, 57_600).map(|t| t.to_string()).as_deref(),
        Some("00:00:00+16")
    );
    assert_eq!(at(0, 57_601), None);
    assert_eq!(at(0, -57_601), None);
    assert_eq!(
        at(0, 19_815).map(|t| t.to_string()).as_deref(),
        Some("00:00:00+05:30:15")
    );
    assert_eq!(
        PgTimeTz::try_from((0, 57_601)),
        Err(DateTimeError::TimeZoneDisplacementOutOfRange)
    );
    assert_eq!(
        PgTimeTz::try_from((-1, 0)),
        Err(DateTimeError::FieldValueOutOfRange)
    );
    assert_eq!(<(i64, i32)>::from(rome), (43_200_000_000, 3_600));
    assert_eq!(PgTimeTz::try_from((43_200_000_000, 3_600)), Ok(rome));
}

#[test]
fn timestamps_hold_postgresql_range_and_infinities() {
    let first = PgTimestamp::from_pg_micros(FIRST_MICRO).expect("first");
    let last = PgTimestamp::from_pg_micros(LAST_MICRO).expect("last");
    assert_eq!(first.to_string(), "4714-11-24 00:00:00 BC");
    assert_eq!(last.to_string(), "294276-12-31 23:59:59.999999");
    assert_eq!(PgTimestamp::from_pg_micros(FIRST_MICRO - 1), None);
    assert_eq!(PgTimestamp::from_pg_micros(LAST_MICRO + 1), None);
    assert_eq!(PgTimestamp::try_from(i64::MAX), Ok(PgTimestamp::INFINITY));
    assert_eq!(
        PgTimestamp::try_from(LAST_MICRO + 1),
        Err(DateTimeError::TimestampOutOfRange)
    );
    assert_eq!(i64::from(last), LAST_MICRO);
    assert_eq!(last.date().to_string(), "294276-12-31");
    assert_eq!(
        last.time().map(|t| t.to_string()).as_deref(),
        Some("23:59:59.999999")
    );
    assert_eq!(first.date().pg_days(), FIRST_DAY);
    assert_eq!(PgTimestamp::INFINITY.date(), PgDate::INFINITY);
    assert_eq!(PgTimestamp::NEG_INFINITY.date(), PgDate::NEG_INFINITY);
    assert_eq!(PgTimestamp::INFINITY.time(), None);
    assert_eq!(PgTimestamp::NEG_INFINITY.to_string(), "-infinity");
    assert!(PgTimestamp::NEG_INFINITY < first && last < PgTimestamp::INFINITY);

    let date = PgDate::from_ymd(294_276, 12, 31).unwrap();
    let end = PgTime::from_micros(86_400_000_000).unwrap();
    assert_eq!(PgTimestamp::new(date, end), None);
    assert_eq!(PgTimestamp::new(PgDate::INFINITY, PgTime::MIDNIGHT), None);
    assert_eq!(
        PgTimestamp::new(PgDate::from_ymd(300_000, 1, 1).unwrap(), PgTime::MIDNIGHT),
        None
    );
    let leap = PgTimestamp::new(PgDate::from_ymd(2024, 2, 28).unwrap(), end).unwrap();
    assert_eq!(leap.to_string(), "2024-02-29 00:00:00");
}

#[test]
fn timestamptz_holds_postgresql_range_and_prints_at_utc() {
    let first = PgTimestampTz::from_pg_micros(FIRST_MICRO).expect("first");
    let last = PgTimestampTz::try_from(LAST_MICRO).expect("last");
    assert_eq!(first.to_string(), "4714-11-24 00:00:00+00 BC");
    assert_eq!(last.to_string(), "294276-12-31 23:59:59.999999+00");
    assert_eq!(PgTimestampTz::from_pg_micros(FIRST_MICRO - 1), None);
    assert_eq!(
        PgTimestampTz::try_from(LAST_MICRO + 1),
        Err(DateTimeError::TimestampOutOfRange)
    );
    assert_eq!(i64::from(last), LAST_MICRO);
    assert_eq!(last.pg_micros(), LAST_MICRO);
    assert_eq!(PgTimestampTz::INFINITY.to_string(), "infinity");
    assert!(PgTimestampTz::NEG_INFINITY < first && !PgTimestampTz::NEG_INFINITY.is_finite());
    assert!(!PgTimestampTz::INFINITY.is_finite() && last.is_finite());
}

/// Zones are equal when `SHOW timezone` names them alike, whatever the spelling they were read
/// from.
#[test]
fn time_zones_equal_by_canonical_name() {
    let zone =
        |setting| PgTimeZone::parse(setting).unwrap_or_else(|error| panic!("{setting}: {error}"));
    assert_eq!(zone("europe/rome"), zone("Europe/Rome"));
    assert_eq!(zone("+2"), zone("02"));
    assert_eq!(zone("abc3"), zone("ABC3"));
    assert_ne!(zone("Europe/Rome"), zone("Europe/Paris"));
    assert_ne!(zone("+02"), zone("UTC-2"));
    let zones: HashSet<PgTimeZone> = ["Europe/Rome", "EUROPE/ROME", "+02", "2"].map(zone).into();
    assert_eq!(zones.len(), 2);
}
