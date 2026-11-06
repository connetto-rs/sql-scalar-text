//! Conversions between the PostgreSQL types and jiff's, over the values both hold. jiff's
//! nanoseconds round to PostgreSQL's microsecond as `rint` rounds a fraction PostgreSQL reads,
//! half to even.

use jiff::{
    Timestamp,
    civil::{Date, DateTime, Time},
};

use super::{
    DateTimeError, PgDate, PgTime, PgTimestamp, PgTimestampTz, USECS_PER_DAY, USECS_PER_HOUR,
    USECS_PER_MINUTE, USECS_PER_SEC, is_valid_timestamp, split_clock, tm2timestamp,
};

/// Microseconds from 1970-01-01 to 2000-01-01.
const UNIX_TO_POSTGRES_MICROS: i64 = 946_684_800_000_000;

/// Nanoseconds rounded to whole microseconds, half to even.
fn round_nanos(nanos: i128) -> i128 {
    let (micros, remainder) = (nanos.div_euclid(1_000), nanos.rem_euclid(1_000));
    if remainder > 500 || (remainder == 500 && micros % 2 != 0) {
        micros + 1
    } else {
        micros
    }
}

/// The microseconds of a jiff time of day, up to 24:00:00 after rounding.
fn time_micros(time: Time) -> i64 {
    let nanos = i128::from(time.hour()) * i128::from(USECS_PER_HOUR) * 1_000
        + i128::from(time.minute()) * i128::from(USECS_PER_MINUTE) * 1_000
        + i128::from(time.second()) * i128::from(USECS_PER_SEC) * 1_000
        + i128::from(time.subsec_nanosecond());
    i64::try_from(round_nanos(nanos)).expect("a day's microseconds fit i64")
}

/// A jiff year, within -9999 to 9999, or `None` outside it.
fn jiff_year(year: i32) -> Option<i16> {
    i16::try_from(year)
        .ok()
        .filter(|year| (-9999..=9999).contains(year))
}

/// The jiff date of a finite year, month and day.
fn jiff_date(year: i32, month: i32, day: i32) -> Result<Date, DateTimeError> {
    let year = jiff_year(year).ok_or(DateTimeError::OutsideJiffRange)?;
    let month = i8::try_from(month).expect("a month fits i8");
    let day = i8::try_from(day).expect("a day fits i8");
    Date::new(year, month, day).map_err(|_| DateTimeError::OutsideJiffRange)
}

/// The jiff time of a clock below 24:00:00.
fn jiff_time(micros: i64) -> Time {
    let (hour, minute, second, micro) = split_clock(micros);
    let narrow = |field: i64| i8::try_from(field).expect("a clock field below 24:00:00 fits i8");
    let nanos = i32::try_from(micro * 1_000).expect("a second's nanoseconds fit i32");
    Time::new(narrow(hour), narrow(minute), narrow(second), nanos)
        .expect("a clock below 24:00:00 is a jiff time")
}

/// Refuses a value jiff does not hold, `infinity` or `-infinity`, with jiff's bound.
impl TryFrom<PgDate> for Date {
    type Error = DateTimeError;

    fn try_from(date: PgDate) -> Result<Self, Self::Error> {
        let (year, month, day) = date.ymd().ok_or(DateTimeError::OutsideJiffRange)?;
        jiff_date(year, month.into(), day.into())
    }
}

/// Refuses a date before PostgreSQL's 4714-11-24 BC.
impl TryFrom<Date> for PgDate {
    type Error = DateTimeError;

    fn try_from(date: Date) -> Result<Self, Self::Error> {
        let month = u8::try_from(date.month()).expect("a jiff month is positive");
        let day = u8::try_from(date.day()).expect("a jiff day is positive");
        Self::from_ymd(date.year().into(), month, day).ok_or(DateTimeError::DateOutOfRange)
    }
}

/// Refuses 24:00:00, which jiff's clock stops short of.
impl TryFrom<PgTime> for Time {
    type Error = DateTimeError;

    fn try_from(time: PgTime) -> Result<Self, Self::Error> {
        if time.0 == USECS_PER_DAY {
            return Err(DateTimeError::FieldValueOutOfRange);
        }
        Ok(jiff_time(time.0))
    }
}

/// A clock within 500 nanoseconds of midnight rounds up to 24:00:00, which PostgreSQL holds.
impl From<Time> for PgTime {
    fn from(time: Time) -> Self {
        Self(time_micros(time))
    }
}

/// Refuses a value jiff does not hold, `infinity` or `-infinity`, with jiff's bound.
impl TryFrom<PgTimestamp> for DateTime {
    type Error = DateTimeError;

    fn try_from(at: PgTimestamp) -> Result<Self, Self::Error> {
        let date = Date::try_from(at.date())?;
        let time = at.time().ok_or(DateTimeError::OutsideJiffRange)?;
        Ok(Self::from_parts(date, jiff_time(time.0)))
    }
}

/// Refuses a timestamp before PostgreSQL's 4714-11-24 BC.
impl TryFrom<DateTime> for PgTimestamp {
    type Error = DateTimeError;

    fn try_from(at: DateTime) -> Result<Self, Self::Error> {
        let (year, month, day) = (
            i32::from(at.year()),
            i32::from(at.month()),
            i32::from(at.day()),
        );
        tm2timestamp(year, month, day, time_micros(at.time()), 0)
            .map(Self)
            .ok_or(DateTimeError::TimestampOutOfRange)
    }
}

/// Refuses a value jiff does not hold, `infinity` or `-infinity`, with jiff's bound.
impl TryFrom<PgTimestampTz> for Timestamp {
    type Error = DateTimeError;

    fn try_from(at: PgTimestampTz) -> Result<Self, Self::Error> {
        if !at.is_finite() {
            return Err(DateTimeError::OutsideJiffRange);
        }
        Self::from_microsecond(at.0 + UNIX_TO_POSTGRES_MICROS)
            .map_err(|_| DateTimeError::OutsideJiffRange)
    }
}

/// Refuses an instant before PostgreSQL's 4714-11-24 BC.
impl TryFrom<Timestamp> for PgTimestampTz {
    type Error = DateTimeError;

    fn try_from(at: Timestamp) -> Result<Self, Self::Error> {
        let micros = i64::try_from(round_nanos(at.as_nanosecond()))
            .expect("jiff's instants lie within i64 microseconds")
            - UNIX_TO_POSTGRES_MICROS;
        if is_valid_timestamp(micros) {
            Ok(Self(micros))
        } else {
            Err(DateTimeError::TimestampOutOfRange)
        }
    }
}
