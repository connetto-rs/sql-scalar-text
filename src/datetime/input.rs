//! PostgreSQL 18's `date_in`, `time_in`, `timetz_in`, `timestamp_in` and `timestamptz_in`, and
//! the readers of the text PostgreSQL, MySQL and SQLite print.

use super::{
    DateTimeError, POSTGRES_EPOCH_JDATE, PgDate, PgTime, PgTimeTz, PgTimestamp, PgTimestampTz,
    USECS_PER_DAY, USECS_PER_HOUR, USECS_PER_MINUTE, USECS_PER_SEC, abbreviation_offset, date2j,
    days_in_month, is_valid_julian, local_offset, tm2timestamp,
};
use super::{
    decode::{
        DateOrder, DateType, DecodedTime, Dterr, Tm, ZoneSource, current_time, decode_date_time,
        decode_time_only,
    },
    tokens::{Tokens, parse_date_time, parse_fractional_second, strtoint},
};
use crate::{PgTimeZone, PgType, TimeZoneAbbreviations, zone::DEFAULT_ABBREVIATIONS};

/// 1970-01-01 00:00:00, `epoch`, in microseconds from 2000-01-01.
const EPOCH_MICROS: i64 = -946_684_800_000_000;
/// 1970-01-01, `epoch`, in days from 2000-01-01.
const EPOCH_DAYS: i32 = -10_957;

/// The session state PostgreSQL's input functions read, which the caller supplies.
///
/// ```
/// use sql_values::{DateTimeError, InputContext, PgTimeZone, PgTimestampTz, TimeZoneAbbreviations};
///
/// let rome = PgTimeZone::parse("Europe/Rome").unwrap();
/// let local = "2026-07-01 12:00:00";
/// assert_eq!(
///     PgTimestampTz::parse(local, InputContext::new()),
///     Err(DateTimeError::NeedsTimeZone),
/// );
/// let at = PgTimestampTz::parse(local, InputContext::new().with_time_zone(&rome)).unwrap();
/// assert_eq!(at.to_string(), "2026-07-01 10:00:00+00");
///
/// // `EST` is New York's time in `Default` and Brisbane's in `Australia`.
/// let australia = TimeZoneAbbreviations::bundled("Australia").unwrap();
/// let context = InputContext::new().with_time_zone_abbreviations(australia);
/// let est = PgTimestampTz::parse("2026-07-01 12:00 EST", context).unwrap();
/// assert_eq!(est.to_string(), "2026-07-01 02:00:00+00");
/// ```
#[derive(Debug, Clone, Copy)]
pub struct InputContext<'a> {
    pub(super) time_zone: Option<&'a PgTimeZone>,
    pub(super) abbreviations: &'a TimeZoneAbbreviations,
    pub(super) date_order: Option<DateOrder>,
    pub(super) now: Option<PgTimestampTz>,
}

impl<'a> InputContext<'a> {
    /// A context with no time zone, no `DateStyle` order and no current time, and the `Default`
    /// abbreviations, as a stock installation's `timezone_abbreviations` names them.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            time_zone: None,
            abbreviations: &DEFAULT_ABBREVIATIONS,
            date_order: None,
            now: None,
        }
    }

    /// The context with `zone` as the session's `TimeZone`, which reads a `timestamptz` or
    /// `timetz` written without an offset, and whose own abbreviations a literal may write
    /// before those of the set.
    #[must_use]
    pub const fn with_time_zone(self, zone: &'a PgTimeZone) -> Self {
        Self {
            time_zone: Some(zone),
            ..self
        }
    }

    /// The context with `abbreviations` as the session's `timezone_abbreviations`.
    #[must_use]
    pub const fn with_time_zone_abbreviations(
        self,
        abbreviations: &'a TimeZoneAbbreviations,
    ) -> Self {
        Self {
            abbreviations,
            ..self
        }
    }

    /// The context with `order` as the field order of the session's `DateStyle`, which reads a
    /// date written in numbers alone such as `01/02/2026`.
    #[must_use]
    pub const fn with_date_order(self, order: DateOrder) -> Self {
        Self {
            date_order: Some(order),
            ..self
        }
    }

    /// The context with `now` as the transaction's start, which `now`, `today`, `tomorrow` and
    /// `yesterday` read, in the context's time zone, and whose date a `timetz` without a date
    /// takes its zone's offset on. An infinite `now` reads as
    /// [`DateTimeError::TimestampOutOfRange`], as no transaction starts at one.
    #[must_use]
    pub const fn with_now(self, now: PgTimestampTz) -> Self {
        Self {
            now: Some(now),
            ..self
        }
    }
}

impl Default for InputContext<'_> {
    fn default() -> Self {
        Self::new()
    }
}

/// `MAXDATELEN`, whose `workbuf` of one byte more bounds a literal's tokens.
const MAX_DATE_LEN: usize = 128;

/// `ParseDateTime` and `decoder`, `DecodeDateTime` or `DecodeTimeOnly`, over a literal of
/// `pg_type`, which a NUL ends as it ends a C string.
fn decode<'a, T>(
    text: &str,
    context: InputContext<'a>,
    pg_type: PgType,
    decoder: fn(&Tokens, InputContext<'a>) -> Result<T, Dterr>,
) -> Result<T, DateTimeError> {
    let text = text.split('\0').next().unwrap_or_default();
    let tokens = parse_date_time(text.as_bytes(), MAX_DATE_LEN + 1)
        .map_err(|_| DateTimeError::InvalidInputSyntax(pg_type))?;
    decoder(&tokens, context).map_err(|error| error.for_type(pg_type))
}

/// `read` under the context's `DateStyle` order. Without one, a literal the order touches reads
/// only if every order reads it alike, as `January 8, 1999` does.
fn by_date_order<'a, T: PartialEq>(
    context: InputContext<'a>,
    read: impl Fn(InputContext<'a>) -> Result<T, DateTimeError>,
) -> Result<T, DateTimeError> {
    match read(context) {
        Err(DateTimeError::NeedsDateOrder) => {
            let [mdy, dmy, ymd] = [DateOrder::Mdy, DateOrder::Dmy, DateOrder::Ymd]
                .map(|order| read(context.with_date_order(order)));
            if mdy == dmy && dmy == ymd {
                mdy
            } else {
                Err(DateTimeError::NeedsDateOrder)
            }
        }
        other => other,
    }
}

/// The local clock of `tm` to the second, as `DetermineTimeZoneOffset` reads it, in microseconds
/// from 2000-01-01, or `None` where that overflows.
fn local_seconds(tm: &Tm) -> Option<i64> {
    let days = i64::from(date2j(tm.year, tm.mon, tm.mday)) - i64::from(POSTGRES_EPOCH_JDATE);
    let clock = (i64::from(tm.hour) * 60 + i64::from(tm.min)) * 60 + i64::from(tm.sec);
    days.checked_mul(USECS_PER_DAY)?
        .checked_add(clock * USECS_PER_SEC)
}

/// `DetermineTimeZoneOffset` and `DetermineTimeZoneAbbrevOffset` for a `timetz`, whose date
/// may lie anywhere `ValidateDate` accepts. Outside `IS_VALID_JULIAN` PostgreSQL takes UTC and
/// reads the abbreviation at the Unix epoch, and a date it can place that jiff cannot is
/// [`DateTimeError::OutsideJiffRange`].
fn time_zone_offset(
    zone: &PgTimeZone,
    abbreviation: Option<&str>,
    tm: &Tm,
) -> Result<i32, DateTimeError> {
    if !is_valid_julian(tm.year, tm.mon) {
        return match abbreviation {
            Some(abbreviation) => Ok(zone
                .interpret_abbreviation(abbreviation, 0)?
                .map_or(0, |meaning| meaning.offset)),
            None => Ok(0),
        };
    }
    let local = local_seconds(tm).ok_or(DateTimeError::OutsideJiffRange)?;
    match abbreviation {
        Some(abbreviation) => abbreviation_offset(zone, abbreviation, local),
        None => local_offset(zone, local),
    }
    .map_err(|_| DateTimeError::OutsideJiffRange)
}

impl PgDate {
    /// PostgreSQL's `date_in`, reading every spelling `DecodeDateTime` reads, with a clock and
    /// zone read, checked and ignored.
    ///
    /// # Errors
    ///
    /// The [`DateTimeError`] PostgreSQL raises, or the session state the literal needs and the
    /// context lacks, [`DateTimeError::NeedsDateOrder`], [`DateTimeError::NeedsCurrentTime`] or
    /// [`DateTimeError::NeedsTimeZone`].
    ///
    /// ```
    /// use sql_values::{DateOrder, DateTimeError, InputContext, PgDate, PgType};
    ///
    /// let read = |text, context| PgDate::parse(text, context).map(|d| d.to_string());
    /// let context = InputContext::new();
    /// assert_eq!(read("5874897-12-31", context).unwrap(), "5874897-12-31");
    /// assert_eq!(read("January 8, 1999 BC", context).unwrap(), "1999-01-08 BC");
    /// assert_eq!(read("J2451545", context).unwrap(), "2000-01-01");
    /// // Under `YMD` a short number after a month name is the year, which `99` can be.
    /// assert_eq!(read("January 8, 99", context), Err(DateTimeError::NeedsDateOrder));
    /// assert_eq!(read("2024-02-30", context), Err(DateTimeError::FieldValueOutOfRange));
    /// assert_eq!(read("nope", context), Err(DateTimeError::InvalidInputSyntax(PgType::Date)));
    /// assert_eq!(read("01/02/2026", context), Err(DateTimeError::NeedsDateOrder));
    /// let dmy = context.with_date_order(DateOrder::Dmy);
    /// assert_eq!(read("01/02/2026", dmy).unwrap(), "2026-02-01");
    /// ```
    pub fn parse(text: &str, context: InputContext<'_>) -> Result<Self, DateTimeError> {
        by_date_order(context, |context| Self::read(text, context))
    }

    /// [`Self::parse`] under the context's order alone.
    fn read(text: &str, context: InputContext<'_>) -> Result<Self, DateTimeError> {
        let decoded = decode(text, context, PgType::Date, decode_date_time)?;
        let tm = match decoded.dtype {
            DateType::Date => decoded.tm,
            DateType::Epoch => return Ok(Self(EPOCH_DAYS)),
            DateType::Late => return Ok(Self::INFINITY),
            DateType::Early => return Ok(Self::NEG_INFINITY),
        };
        if !is_valid_julian(tm.year, tm.mon) {
            return Err(DateTimeError::DateOutOfRange);
        }
        Self::from_pg_days(date2j(tm.year, tm.mon, tm.mday) - POSTGRES_EPOCH_JDATE)
            .filter(|date| date.is_finite())
            .ok_or(DateTimeError::DateOutOfRange)
    }
}

impl PgTimestamp {
    /// PostgreSQL's `timestamp_in`, reading every spelling `DecodeDateTime` reads, with a zone
    /// read, checked and ignored.
    ///
    /// # Errors
    ///
    /// The [`DateTimeError`] PostgreSQL raises, or the session state the literal needs and the
    /// context lacks.
    ///
    /// ```
    /// use sql_values::{DateTimeError, InputContext, PgTimestamp};
    ///
    /// let read = |text| PgTimestamp::parse(text, InputContext::new()).map(|t| t.to_string());
    /// assert_eq!(read("2024-12-31 24:00:00").unwrap(), "2025-01-01 00:00:00");
    /// assert_eq!(read("Jan 1999 08 04:05:06 PM").unwrap(), "1999-01-08 16:05:06");
    /// assert_eq!(read("19990108T040506.5").unwrap(), "1999-01-08 04:05:06.5");
    /// assert_eq!(read("294277-01-01"), Err(DateTimeError::TimestampOutOfRange));
    /// ```
    pub fn parse(text: &str, context: InputContext<'_>) -> Result<Self, DateTimeError> {
        by_date_order(context, |context| Self::read(text, context))
    }

    /// [`Self::parse`] under the context's order alone.
    fn read(text: &str, context: InputContext<'_>) -> Result<Self, DateTimeError> {
        let decoded = decode(text, context, PgType::Timestamp, decode_date_time)?;
        match decoded.dtype {
            DateType::Date => {
                let tm = decoded.tm;
                tm2timestamp(tm.year, tm.mon, tm.mday, tm.clock(), 0)
                    .map(Self)
                    .ok_or(DateTimeError::TimestampOutOfRange)
            }
            DateType::Epoch => Ok(Self(EPOCH_MICROS)),
            DateType::Late => Ok(Self::INFINITY),
            DateType::Early => Ok(Self::NEG_INFINITY),
        }
    }
}

impl PgTimestampTz {
    /// PostgreSQL's `timestamptz_in`, reading every spelling `DecodeDateTime` reads. A zone the
    /// literal names, or else the context's time zone, decides the offset of a literal without
    /// a numeric one, its local clock resolved as [`PgTimestamp::to_timestamptz`] resolves it,
    /// and an abbreviation whose meaning changed takes the one it had at that clock.
    ///
    /// # Errors
    ///
    /// The [`DateTimeError`] PostgreSQL raises, the session state the literal needs and the
    /// context lacks, or [`DateTimeError::OutsideJiffRange`] where an IANA zone is asked about
    /// a clock outside jiff's years.
    ///
    /// ```
    /// use sql_values::{DateTimeError, InputContext, PgTimeZone, PgTimestampTz};
    ///
    /// let read = |text, context| PgTimestampTz::parse(text, context).map(|t| t.to_string());
    /// let context = InputContext::new();
    /// assert_eq!(read("2024-01-01 12:00+15:59:59", context).unwrap(), "2023-12-31 20:00:01+00");
    /// assert_eq!(
    ///     read("2024-01-01 12:00+16", context),
    ///     Err(DateTimeError::TimeZoneDisplacementOutOfRange),
    /// );
    /// // `MSK` follows Europe/Moscow, which kept +04 from 2011 to 2014.
    /// assert_eq!(read("2012-01-01 12:00 MSK", context).unwrap(), "2012-01-01 08:00:00+00");
    /// assert_eq!(read("2015-01-01 12:00 MSK", context).unwrap(), "2015-01-01 09:00:00+00");
    /// assert_eq!(read("2026-07-01 12:00 Europe/Rome", context).unwrap(), "2026-07-01 10:00:00+00");
    /// assert_eq!(read("now", context), Err(DateTimeError::NeedsCurrentTime));
    /// let rome = PgTimeZone::parse("Europe/Rome").unwrap();
    /// let start = PgTimestampTz::parse("2026-07-01 23:30+00", context).unwrap();
    /// let session = context.with_time_zone(&rome).with_now(start);
    /// assert_eq!(read("today", session).unwrap(), "2026-07-01 22:00:00+00");
    /// ```
    pub fn parse(text: &str, context: InputContext<'_>) -> Result<Self, DateTimeError> {
        by_date_order(context, |context| Self::read(text, context))
    }

    /// [`Self::parse`] under the context's order alone.
    fn read(text: &str, context: InputContext<'_>) -> Result<Self, DateTimeError> {
        let decoded = decode(text, context, PgType::TimestampTz, decode_date_time)?;
        let tm = match decoded.dtype {
            DateType::Date => decoded.tm,
            DateType::Epoch => return Ok(Self(EPOCH_MICROS)),
            DateType::Late => return Ok(Self::INFINITY),
            DateType::Early => return Ok(Self::NEG_INFINITY),
        };
        let out_of_range = DateTimeError::TimestampOutOfRange;
        // The local clock as microseconds from 2000-01-01, for a zone to resolve.
        let local = || {
            if !is_valid_julian(tm.year, tm.mon) {
                return Err(out_of_range);
            }
            let days =
                i64::from(date2j(tm.year, tm.mon, tm.mday)) - i64::from(POSTGRES_EPOCH_JDATE);
            days.checked_mul(USECS_PER_DAY)
                .and_then(|micros| micros.checked_add(tm.clock()))
                .ok_or(out_of_range)
        };
        let offset = match &decoded.zone {
            ZoneSource::West(west) => -*west,
            ZoneSource::Abbreviation { zone, upper } => abbreviation_offset(zone, upper, local()?)?,
            ZoneSource::Named(zone) => local_offset(zone, local()?)?,
            ZoneSource::Session => {
                let zone = context.time_zone.ok_or(DateTimeError::NeedsTimeZone)?;
                local_offset(zone, local()?)?
            }
        };
        tm2timestamp(tm.year, tm.mon, tm.mday, tm.clock(), offset)
            .map(Self)
            .ok_or(out_of_range)
    }
}

impl PgTime {
    /// PostgreSQL's `time_in`, reading every spelling `DecodeTimeOnly` reads, with a date and
    /// zone read, checked and ignored, and the fraction rounded to the microsecond.
    ///
    /// # Errors
    ///
    /// The [`DateTimeError`] PostgreSQL raises, or the session state the literal needs and the
    /// context lacks.
    ///
    /// ```
    /// use sql_values::{DateTimeError, InputContext, PgTime, PgType};
    ///
    /// let read = |text| PgTime::parse(text, InputContext::new()).map(|t| t.to_string());
    /// assert_eq!(read("23:59:60").unwrap(), "24:00:00");
    /// assert_eq!(read("12:00:00.1234565").unwrap(), "12:00:00.123456");
    /// assert_eq!(read("040506.5 PM").unwrap(), "16:05:06.5");
    /// assert_eq!(read("T12:00 Europe/Rome"), Err(DateTimeError::InvalidInputSyntax(PgType::Time)));
    /// assert_eq!(read("2026-07-01 12:00 Europe/Rome").unwrap(), "12:00:00");
    /// assert_eq!(read("24:00:00.000001"), Err(DateTimeError::FieldValueOutOfRange));
    /// ```
    pub fn parse(text: &str, context: InputContext<'_>) -> Result<Self, DateTimeError> {
        by_date_order(context, |context| Self::read(text, context))
    }

    /// [`Self::parse`] under the context's order alone.
    fn read(text: &str, context: InputContext<'_>) -> Result<Self, DateTimeError> {
        let decoded = decode(text, context, PgType::Time, decode_time_only)?;
        Ok(Self(decoded.tm.clock()))
    }
}

impl PgTimeTz {
    /// PostgreSQL's `timetz_in`, reading every spelling `DecodeTimeOnly` reads. A time without
    /// an offset, or with a zone whose offset changes, takes the zone's offset on the date the
    /// literal writes, or else on the current date in the context's time zone.
    ///
    /// # Errors
    ///
    /// The [`DateTimeError`] PostgreSQL raises, the session state the literal needs and the
    /// context lacks, or [`DateTimeError::OutsideJiffRange`] where an IANA zone is asked about
    /// a date outside jiff's years.
    ///
    /// ```
    /// use sql_values::{DateTimeError, InputContext, PgTimeTz, PgTimeZone, PgTimestampTz};
    ///
    /// let read = |text, context| PgTimeTz::parse(text, context).map(|t| t.to_string());
    /// let context = InputContext::new();
    /// assert_eq!(read("12:00:00-05:30", context).unwrap(), "12:00:00-05:30");
    /// assert_eq!(read("allballs", context).unwrap(), "00:00:00+00");
    /// assert_eq!(read("2026-07-01 12:00 Europe/Rome", context).unwrap(), "12:00:00+02");
    /// assert_eq!(read("12:00", context), Err(DateTimeError::NeedsCurrentTime));
    /// let rome = PgTimeZone::parse("Europe/Rome").unwrap();
    /// let start = PgTimestampTz::parse("2026-01-15 12:00+00", context).unwrap();
    /// let session = context.with_time_zone(&rome).with_now(start);
    /// assert_eq!(read("12:00", session).unwrap(), "12:00:00+01");
    /// ```
    pub fn parse(text: &str, context: InputContext<'_>) -> Result<Self, DateTimeError> {
        by_date_order(context, |context| Self::read(text, context))
    }

    /// [`Self::parse`] under the context's order alone.
    fn read(text: &str, context: InputContext<'_>) -> Result<Self, DateTimeError> {
        let pg_type = PgType::TimeTz;
        let DecodedTime { tm, dated, zone } = decode(text, context, pg_type, decode_time_only)?;
        // The literal's date, or the current one, at the literal's clock.
        let at = || {
            let mut at = if dated {
                tm
            } else {
                current_time(context)
                    .map_err(|error| error.for_type(pg_type))?
                    .0
            };
            (at.hour, at.min, at.sec) = (tm.hour, tm.min, tm.sec);
            Ok::<_, DateTimeError>(at)
        };
        let offset = match &zone {
            ZoneSource::West(west) => -*west,
            ZoneSource::Named(zone) => time_zone_offset(zone, None, &tm)?,
            ZoneSource::Abbreviation { zone, upper } => {
                time_zone_offset(zone, Some(upper), &at()?)?
            }
            ZoneSource::Session => {
                let at = at()?;
                let zone = context.time_zone.ok_or(DateTimeError::NeedsTimeZone)?;
                time_zone_offset(zone, None, &at)?
            }
        };
        Ok(Self {
            time: tm.clock(),
            offset,
        })
    }
}

/// Parse `YYYY-MM-DD HH:MM:SS[.f]` with a space or `T` separator, a year of four or more digits,
/// PostgreSQL's trailing ` BC`, and `infinity` and `-infinity`, the fraction rounded to the
/// microsecond. Year `0000`, as MySQL and SQLite print 1 BC, is year 0.
///
/// ```
/// let at = sql_values::parse_timestamp("2024-01-02 03:04:05.125").unwrap();
/// assert_eq!(at.to_string(), "2024-01-02 03:04:05.125");
/// ```
#[must_use]
pub fn parse_timestamp(text: &str) -> Option<PgTimestamp> {
    match text {
        "infinity" => return Some(PgTimestamp::INFINITY),
        "-infinity" => return Some(PgTimestamp::NEG_INFINITY),
        _ => {}
    }
    let (body, bc) = split_era(text.as_bytes());
    match parse_datetime_prefix(body, bc)? {
        (date, clock, []) => date.at(clock, 0).map(PgTimestamp),
        _ => None,
    }
}

/// Parse a [`parse_timestamp`] layout followed by a `Z`, `±HH`, `±HHMM`, `±HH:MM` or `±HH:MM:SS`
/// offset, then PostgreSQL's optional ` BC`, or `infinity` and `-infinity`.
///
/// ```
/// let at = sql_values::parse_timestamp_tz("2024-01-02T03:04:05+02:00").unwrap();
/// assert_eq!(at.to_string(), "2024-01-02 01:04:05+00");
/// ```
#[must_use]
pub fn parse_timestamp_tz(text: &str) -> Option<PgTimestampTz> {
    match text {
        "infinity" => return Some(PgTimestampTz::INFINITY),
        "-infinity" => return Some(PgTimestampTz::NEG_INFINITY),
        _ => {}
    }
    let (body, bc) = split_era(text.as_bytes());
    let (date, clock, offset) = parse_datetime_prefix(body, bc)?;
    date.at(clock, parse_offset(offset)?).map(PgTimestampTz)
}

/// Parse a `YYYY-MM-DD` date with a year of four or more digits and PostgreSQL's trailing ` BC`,
/// or `infinity` and `-infinity`. Year `0000`, as MySQL and SQLite print 1 BC, is year 0.
///
/// ```
/// let d = sql_values::parse_date("2024-01-02").unwrap();
/// assert_eq!(d.to_string(), "2024-01-02");
/// ```
#[must_use]
pub fn parse_date(text: &str) -> Option<PgDate> {
    match text {
        "infinity" => return Some(PgDate::INFINITY),
        "-infinity" => return Some(PgDate::NEG_INFINITY),
        _ => {}
    }
    let (body, bc) = split_era(text.as_bytes());
    match parse_date_prefix(body, bc)? {
        (date, []) => PgDate::from_ymd(date.year, date.month, date.day),
        _ => None,
    }
}

/// Parse `HH:MM:SS[.f]` with one to nine fraction digits, rounded to the microsecond, up to
/// 24:00:00.
///
/// ```
/// let t = sql_values::parse_time("03:04:05.125").unwrap();
/// assert_eq!(t.to_string(), "03:04:05.125");
/// ```
#[must_use]
pub fn parse_time(text: &str) -> Option<PgTime> {
    match parse_time_prefix(text.as_bytes())? {
        (micros, []) => Some(PgTime(micros)),
        _ => None,
    }
}

/// Split off PostgreSQL's ` BC`, which trails the whole value.
fn split_era(bytes: &[u8]) -> (&[u8], bool) {
    match bytes.strip_suffix(b" BC") {
        Some(body) => (body, true),
        None => (bytes, false),
    }
}

/// A year, month and day as written, where year `Y` BC is year `1 - Y`.
#[derive(Clone, Copy)]
struct Ymd {
    year: i32,
    month: u8,
    day: u8,
}

impl Ymd {
    /// The timestamp of `clock` on this date with `offset` seconds east subtracted, or `None`
    /// outside the calendar or PostgreSQL's range.
    fn at(self, clock: i64, offset: i32) -> Option<i64> {
        let (month, day) = (i32::from(self.month), i32::from(self.day));
        if !(1..=12).contains(&month) || !(1..=days_in_month(self.year, month)).contains(&day) {
            return None;
        }
        tm2timestamp(self.year, month, day, clock, offset)
    }
}

/// Parse a leading `Y{4,}-MM-DD`, where year `Y` BC is year `1 - Y` and year `0000` is year 0.
fn parse_date_prefix(bytes: &[u8], bc: bool) -> Option<(Ymd, &[u8])> {
    let year_len = bytes.iter().take_while(|b| b.is_ascii_digit()).count();
    if year_len < 4 || (year_len > 4 && bytes[0] == b'0') {
        return None;
    }
    let (year, _, erange) = strtoint(bytes, 0);
    if erange {
        return None;
    }
    let rest = &bytes[year_len..];
    let year = match (year, bc) {
        (0, true) => return None,
        (_, true) => 1 - year,
        (_, false) => year,
    };
    let Some((&[b'-', m1, m2, b'-', d1, d2], rest)) = rest.split_first_chunk() else {
        return None;
    };
    let month = parse_two_digits([m1, m2])?;
    let day = parse_two_digits([d1, d2])?;
    Some((Ymd { year, month, day }, rest))
}

/// Parse a leading `HH:MM:SS[.f]` with one to nine fraction digits, as microseconds up to
/// 24:00:00.
fn parse_time_prefix(bytes: &[u8]) -> Option<(i64, &[u8])> {
    let Some((&[h1, h2, b':', m1, m2, b':', s1, s2], rest)) = bytes.split_first_chunk() else {
        return None;
    };
    let (fsec, rest) = match rest.split_first() {
        Some((b'.', fraction)) => {
            let len = fraction.iter().take_while(|b| b.is_ascii_digit()).count();
            if !(1..=9).contains(&len) {
                return None;
            }
            let fsec = parse_fractional_second(&rest[..=len]).ok()?;
            (i64::from(fsec), &fraction[len..])
        }
        _ => (0, rest),
    };
    let (hour, minute, second) = (
        parse_two_digits([h1, h2])?,
        parse_two_digits([m1, m2])?,
        parse_two_digits([s1, s2])?,
    );
    if minute > 59 || second > 59 {
        return None;
    }
    let micros = i64::from(hour) * USECS_PER_HOUR
        + i64::from(minute) * USECS_PER_MINUTE
        + i64::from(second) * USECS_PER_SEC
        + fsec;
    (micros <= USECS_PER_DAY).then_some((micros, rest))
}

/// Parse a leading date and time joined by a space or `T`, the clock in microseconds.
fn parse_datetime_prefix(bytes: &[u8], bc: bool) -> Option<(Ymd, i64, &[u8])> {
    let (date, rest) = parse_date_prefix(bytes, bc)?;
    let (&(b' ' | b'T'), rest) = rest.split_first()? else {
        return None;
    };
    let (clock, rest) = parse_time_prefix(rest)?;
    Some((date, clock, rest))
}

/// Two ASCII digits as a number.
fn parse_two_digits(digits: [u8; 2]) -> Option<u8> {
    if digits.iter().all(u8::is_ascii_digit) {
        Some((digits[0] - b'0') * 10 + (digits[1] - b'0'))
    } else {
        None
    }
}

/// Parse a `Z`, `±HH`, `±HHMM`, `±HH:MM` or `±HH:MM:SS` offset as seconds east of UTC, with an
/// hour up to 23.
fn parse_offset(bytes: &[u8]) -> Option<i32> {
    if bytes == b"Z" {
        return Some(0);
    }
    let (&sign, digits) = bytes.split_first()?;
    let negative = match sign {
        b'+' => false,
        b'-' => true,
        _ => return None,
    };
    let pair = |at: usize| parse_two_digits(digits.get(at..at + 2)?.try_into().ok()?);
    let hour = pair(0)?;
    let (minute, second) = match digits.len() {
        2 => (0, 0),
        4 => (pair(2)?, 0),
        5 if digits[2] == b':' => (pair(3)?, 0),
        8 if digits[2] == b':' && digits[5] == b':' => (pair(3)?, pair(6)?),
        _ => return None,
    };
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let seconds = (i32::from(hour) * 60 + i32::from(minute)) * 60 + i32::from(second);
    Some(if negative { -seconds } else { seconds })
}
