//! PostgreSQL 18's `date_in`, `time_in`, `timetz_in`, `timestamp_in` and `timestamptz_in` for the
//! ISO spelling, and the readers of the text PostgreSQL, MySQL and SQLite print.

use super::{
    DateTimeError, POSTGRES_EPOCH_JDATE, PgDate, PgTime, PgTimeTz, PgTimestamp, PgTimestampTz,
    USECS_PER_DAY, USECS_PER_HOUR, USECS_PER_MINUTE, USECS_PER_SEC, date2j, days_in_month,
    is_valid_julian, tm2timestamp,
};
use crate::{
    cast::{is_pg_space, trim_pg_space},
    float::{round_ties_even, truncate_to_i64},
};

/// `MAX_TZDISP_HOUR`, the largest hour of an offset a literal writes.
const MAX_TZDISP_HOUR: i32 = 15;
/// The longest literal the readers take, well inside PostgreSQL's `MAXDATELEN` of 128 bytes
/// for the fields they read.
const MAX_LITERAL_LEN: usize = 64;
/// 1970-01-01 00:00:00, `epoch`, in microseconds from 2000-01-01.
const EPOCH_MICROS: i64 = -946_684_800_000_000;
/// 1970-01-01, `epoch`, in days from 2000-01-01.
const EPOCH_DAYS: i32 = -10_957;

/// The reserved words `date_in`, `timestamp_in` and `timestamptz_in` read without a clock.
enum Special {
    /// `infinity` or `+infinity`, `DTK_LATE`.
    Late,
    /// `-infinity`, `DTK_EARLY`.
    Early,
    /// `epoch`, `DTK_EPOCH`.
    Epoch,
}

/// A literal's fields as `DecodeDateTime` leaves them, already checked.
struct Fields {
    /// The proleptic Gregorian year, where year `Y` BC is `1 - Y`.
    year: i32,
    month: i32,
    day: i32,
    /// The clock in microseconds, up to 24:00:00.
    clock: i64,
    /// The offset east of UTC in seconds, when the literal writes one.
    offset: Option<i32>,
}

/// What a date and time literal reads as.
enum Literal {
    Special(Special),
    Fields(Fields),
}

/// An offset as written, before `DecodeTimezone` checks it.
enum RawOffset<'a> {
    /// `Z`, which the `Default` abbreviation set reads as UTC.
    Zulu,
    /// `+` or `-` and the digits and colons after it.
    Numeric { negative: bool, digits: &'a [u8] },
}

/// A clock as written, before `DecodeTime` checks it.
struct RawClock<'a> {
    hour: [u8; 2],
    minute: [u8; 2],
    second: Option<[u8; 2]>,
    /// The fraction from its point, `.` alone included.
    fraction: Option<&'a [u8]>,
    offset: Option<RawOffset<'a>>,
}

/// A byte cursor over a literal.
struct Scanner<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Scanner<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    fn peek_at(&self, ahead: usize) -> Option<u8> {
        self.bytes.get(self.at + ahead).copied()
    }

    fn eat(&mut self, byte: u8) -> bool {
        let found = self.peek() == Some(byte);
        self.at += usize::from(found);
        found
    }

    fn digits(&mut self) -> &'a [u8] {
        let start = self.at;
        while self.peek().is_some_and(|b| b.is_ascii_digit()) {
            self.at += 1;
        }
        &self.bytes[start..self.at]
    }

    fn two_digits(&mut self) -> Option<[u8; 2]> {
        match (self.peek(), self.peek_at(1)) {
            (Some(a), Some(b)) if a.is_ascii_digit() && b.is_ascii_digit() => {
                self.at += 2;
                Some([a, b])
            }
            _ => None,
        }
    }

    fn spaces(&mut self) -> usize {
        let start = self.at;
        while self.peek().is_some_and(is_pg_space) {
            self.at += 1;
        }
        self.at - start
    }

    fn is_done(&self) -> bool {
        self.at == self.bytes.len()
    }

    /// `HH:MM[:SS[.f]]` and an optional offset after blanks.
    fn clock(&mut self) -> Option<RawClock<'a>> {
        let hour = self.two_digits()?;
        if !self.eat(b':') {
            return None;
        }
        let minute = self.two_digits()?;
        let mut second = None;
        let mut fraction = None;
        if self.eat(b':') {
            second = Some(self.two_digits()?);
            if self.peek() == Some(b'.') {
                let start = self.at;
                self.at += 1;
                self.digits();
                fraction = Some(&self.bytes[start..self.at]);
            }
        }
        if self.peek().is_some_and(|b| b.is_ascii_digit() || b == b'.') {
            return None;
        }
        let before_offset = self.at;
        self.spaces();
        let offset = match self.peek() {
            Some(b'Z' | b'z') if !self.peek_at(1).is_some_and(|b| b.is_ascii_alphabetic()) => {
                self.at += 1;
                Some(RawOffset::Zulu)
            }
            Some(sign @ (b'+' | b'-')) if self.peek_at(1).is_some_and(|b| b.is_ascii_digit()) => {
                self.at += 1;
                let start = self.at;
                while self.peek().is_some_and(|b| b.is_ascii_digit() || b == b':') {
                    self.at += 1;
                }
                if matches!(self.peek(), Some(b'.' | b'-')) {
                    return None;
                }
                Some(RawOffset::Numeric {
                    negative: sign == b'-',
                    digits: &self.bytes[start..self.at],
                })
            }
            _ => {
                self.at = before_offset;
                None
            }
        };
        Some(RawClock {
            hour,
            minute,
            second,
            fraction,
            offset,
        })
    }

    /// A trailing ` BC` or ` AD`, `true` for BC, or `None` when something else is left.
    fn era(&mut self, needs_space: bool) -> Option<bool> {
        let spaces = self.spaces();
        if self.is_done() {
            return Some(false);
        }
        if needs_space && spaces == 0 {
            return None;
        }
        let rest = &self.bytes[self.at..];
        let bc = if rest.eq_ignore_ascii_case(b"bc") {
            true
        } else if rest.eq_ignore_ascii_case(b"ad") {
            false
        } else {
            return None;
        };
        self.at = self.bytes.len();
        Some(bc)
    }
}

/// `strtoint` of decimal digits, `None` on `ERANGE`.
fn strtoint(digits: &[u8]) -> Option<i32> {
    digits.iter().try_fold(0i32, |value, &digit| {
        value.checked_mul(10)?.checked_add(i32::from(digit - b'0'))
    })
}

/// The value of two ASCII digits.
fn two(digits: [u8; 2]) -> i32 {
    i32::from(digits[0] - b'0') * 10 + i32::from(digits[1] - b'0')
}

/// `ParseFractionalSecond`, a fraction from its point as whole microseconds, `rint` of the
/// `double` times a million.
fn fractional_second(fraction: &[u8]) -> i64 {
    if fraction.len() == 1 {
        return 0;
    }
    let value: f64 = core::str::from_utf8(fraction)
        .ok()
        .and_then(|text| text.parse().ok())
        .expect("a point and digits read as a double");
    truncate_to_i64(round_ties_even(value * 1_000_000.0))
}

impl RawClock<'_> {
    /// `DecodeTime`'s sanity check, the clock in microseconds with the hour unchecked.
    fn decode(&self) -> Result<i64, DateTimeError> {
        let (hour, minute) = (two(self.hour), two(self.minute));
        let second = self.second.map_or(0, two);
        let fsec = self.fraction.map_or(0, fractional_second);
        // A fraction below one rounds to at most a whole second, which PostgreSQL allows.
        if minute > 59 || second > 60 {
            return Err(DateTimeError::FieldValueOutOfRange);
        }
        Ok(i64::from(hour) * USECS_PER_HOUR
            + i64::from(minute) * USECS_PER_MINUTE
            + i64::from(second) * USECS_PER_SEC
            + fsec)
    }
}

/// `time_overflows` on a clock `DecodeTime` accepted, whose minute, second and fraction are each
/// in range but whose total may pass 24:00:00, as any hour past 24 makes it.
fn check_time_overflow(clock: i64) -> Result<i64, DateTimeError> {
    if clock > USECS_PER_DAY {
        Err(DateTimeError::FieldValueOutOfRange)
    } else {
        Ok(clock)
    }
}

/// `DecodeTimezone`, the offset east of UTC in seconds.
fn decode_timezone(offset: &RawOffset<'_>) -> Result<i32, DateTimeError> {
    let (negative, digits) = match *offset {
        RawOffset::Zulu => return Ok(0),
        RawOffset::Numeric { negative, digits } => (negative, digits),
    };
    // The token starts with a digit, so the hour is never empty.
    let mut parts = digits.split(|&b| b == b':');
    let mut field = || match parts.next() {
        None => Ok(0),
        Some([]) => Err(DateTimeError::Unsupported),
        Some(part) => strtoint(part).ok_or(DateTimeError::TimeZoneDisplacementOutOfRange),
    };
    let mut hour = field()?;
    let mut minute = field()?;
    let second = field()?;
    if parts.next().is_some() {
        return Err(DateTimeError::Unsupported);
    }
    let colon = digits.contains(&b':');
    // `strlen(str) > 3` counts the sign, so three or more digits run hours and minutes together.
    if !colon && digits.len() > 2 {
        minute = hour % 100;
        hour /= 100;
    }
    if hour > MAX_TZDISP_HOUR || minute >= 60 || second >= 60 {
        return Err(DateTimeError::TimeZoneDisplacementOutOfRange);
    }
    let seconds = (hour * 60 + minute) * 60 + second;
    Ok(if negative { -seconds } else { seconds })
}

/// `ParseDateTime` and `DecodeDateTime` for the spellings the readers take, the reserved words
/// that need no clock and `YYYY-MM-DD[( |T)HH:MM[:SS[.f]][offset]][ BC|AD]` between blanks.
fn read_date_time(text: &str) -> Result<Literal, DateTimeError> {
    let bytes = trim_pg_space(text.as_bytes());
    if bytes.len() > MAX_LITERAL_LEN {
        return Err(DateTimeError::Unsupported);
    }
    let special =
        if bytes.eq_ignore_ascii_case(b"infinity") || bytes.eq_ignore_ascii_case(b"+infinity") {
            Some(Special::Late)
        } else if bytes.eq_ignore_ascii_case(b"-infinity") {
            Some(Special::Early)
        } else if bytes.eq_ignore_ascii_case(b"epoch") {
            Some(Special::Epoch)
        } else {
            None
        };
    if let Some(special) = special {
        return Ok(Literal::Special(special));
    }

    let unsupported = DateTimeError::Unsupported;
    let mut scanner = Scanner::new(bytes);
    let year = scanner.digits();
    if year.len() < 4 || (year.len() > 4 && year[0] == b'0') || !scanner.eat(b'-') {
        return Err(unsupported);
    }
    let month = scanner.two_digits().ok_or(unsupported)?;
    if !scanner.eat(b'-') {
        return Err(unsupported);
    }
    let day = scanner.two_digits().ok_or(unsupported)?;
    if scanner.peek().is_some_and(|b| b.is_ascii_digit()) {
        return Err(unsupported);
    }
    let after_date = scanner.at;
    let separated = match scanner.peek() {
        Some(b'T' | b't') => {
            scanner.at += 1;
            true
        }
        _ => scanner.spaces() > 0,
    };
    let clock = if separated && scanner.peek().is_some_and(|b| b.is_ascii_digit()) {
        Some(scanner.clock().ok_or(unsupported)?)
    } else {
        scanner.at = after_date;
        None
    };
    let zulu_last = matches!(
        clock.as_ref().and_then(|c| c.offset.as_ref()),
        Some(RawOffset::Zulu)
    );
    let bc = scanner.era(zulu_last).ok_or(unsupported)?;

    // The checks in `DecodeDateTime`'s order, the date token, the time token, the zone token,
    // then `ValidateDate`.
    let year = strtoint(year).ok_or(DateTimeError::FieldValueOutOfRange)?;
    let (clock, offset) = match clock {
        None => (0, None),
        Some(raw) => {
            let clock = check_time_overflow(raw.decode()?)?;
            let offset = raw.offset.as_ref().map(decode_timezone).transpose()?;
            (clock, offset)
        }
    };
    if year <= 0 {
        return Err(DateTimeError::FieldValueOutOfRange);
    }
    let year = if bc { 1 - year } else { year };
    let (month, day) = (two(month), two(day));
    if !(1..=12).contains(&month) || !(1..=days_in_month(year, month)).contains(&day) {
        return Err(DateTimeError::FieldValueOutOfRange);
    }
    Ok(Literal::Fields(Fields {
        year,
        month,
        day,
        clock,
        offset,
    }))
}

/// `DecodeTimeOnly` for the spellings the readers take, `allballs` and `HH:MM[:SS[.f]]` with
/// an optional offset between blanks. `None` for `allballs`.
fn read_time(text: &str) -> Result<Option<(i64, Option<i32>)>, DateTimeError> {
    let bytes = trim_pg_space(text.as_bytes());
    if bytes.len() > MAX_LITERAL_LEN {
        return Err(DateTimeError::Unsupported);
    }
    if bytes.eq_ignore_ascii_case(b"allballs") {
        return Ok(None);
    }
    let mut scanner = Scanner::new(bytes);
    let raw = scanner.clock().ok_or(DateTimeError::Unsupported)?;
    if !scanner.is_done() {
        return Err(DateTimeError::Unsupported);
    }
    let clock = raw.decode()?;
    let offset = raw.offset.as_ref().map(decode_timezone).transpose()?;
    Ok(Some((check_time_overflow(clock)?, offset)))
}

impl PgDate {
    /// PostgreSQL's `date_in`, reading the ISO spelling and `infinity`, `-infinity` and `epoch`,
    /// with a clock and offset read, checked and ignored.
    ///
    /// # Errors
    ///
    /// The [`DateTimeError`] PostgreSQL raises, or [`DateTimeError::Unsupported`] for another
    /// spelling.
    ///
    /// ```
    /// use sql_values::{DateTimeError, PgDate};
    ///
    /// let read = |text| PgDate::parse(text).map(|d| d.to_string());
    /// assert_eq!(read("5874897-12-31").unwrap(), "5874897-12-31");
    /// assert_eq!(read(" -Infinity ").unwrap(), "-infinity");
    /// assert_eq!(read("2024-02-30"), Err(DateTimeError::FieldValueOutOfRange));
    /// assert_eq!(read("4714-11-23 BC"), Err(DateTimeError::DateOutOfRange));
    /// ```
    pub fn parse(text: &str) -> Result<Self, DateTimeError> {
        let fields = match read_date_time(text)? {
            Literal::Special(Special::Late) => return Ok(Self::INFINITY),
            Literal::Special(Special::Early) => return Ok(Self::NEG_INFINITY),
            Literal::Special(Special::Epoch) => return Ok(Self(EPOCH_DAYS)),
            Literal::Fields(fields) => fields,
        };
        if !is_valid_julian(fields.year, fields.month) {
            return Err(DateTimeError::DateOutOfRange);
        }
        Self::from_pg_days(date2j(fields.year, fields.month, fields.day) - POSTGRES_EPOCH_JDATE)
            .filter(|date| date.is_finite())
            .ok_or(DateTimeError::DateOutOfRange)
    }
}

impl PgTimestamp {
    /// PostgreSQL's `timestamp_in`, reading the ISO spelling and `infinity`, `-infinity` and
    /// `epoch`, with an offset read, checked and ignored.
    ///
    /// # Errors
    ///
    /// The [`DateTimeError`] PostgreSQL raises, or [`DateTimeError::Unsupported`] for another
    /// spelling.
    ///
    /// ```
    /// use sql_values::{DateTimeError, PgTimestamp};
    ///
    /// let read = |text| PgTimestamp::parse(text).map(|t| t.to_string());
    /// assert_eq!(read("2024-12-31 24:00:00").unwrap(), "2025-01-01 00:00:00");
    /// assert_eq!(read("4714-11-24 00:00:00 BC").unwrap(), "4714-11-24 00:00:00 BC");
    /// assert_eq!(read("294277-01-01"), Err(DateTimeError::TimestampOutOfRange));
    /// ```
    pub fn parse(text: &str) -> Result<Self, DateTimeError> {
        match read_date_time(text)? {
            Literal::Special(Special::Late) => Ok(Self::INFINITY),
            Literal::Special(Special::Early) => Ok(Self::NEG_INFINITY),
            Literal::Special(Special::Epoch) => Ok(Self(EPOCH_MICROS)),
            Literal::Fields(fields) => {
                tm2timestamp(fields.year, fields.month, fields.day, fields.clock, 0)
                    .map(Self)
                    .ok_or(DateTimeError::TimestampOutOfRange)
            }
        }
    }
}

impl PgTimestampTz {
    /// PostgreSQL's `timestamptz_in`, reading the ISO spelling and `infinity`, `-infinity` and
    /// `epoch`.
    ///
    /// # Errors
    ///
    /// The [`DateTimeError`] PostgreSQL raises, [`DateTimeError::NeedsTimeZone`] for a literal
    /// without an offset, whose instant the session's `TimeZone` decides, or
    /// [`DateTimeError::Unsupported`] for another spelling.
    ///
    /// ```
    /// use sql_values::{DateTimeError, PgTimestampTz};
    ///
    /// let read = |text| PgTimestampTz::parse(text).map(|t| t.to_string());
    /// assert_eq!(read("2024-01-01 12:00+15:59:59").unwrap(), "2023-12-31 20:00:01+00");
    /// assert_eq!(read("2024-01-01 12:00+16"), Err(DateTimeError::TimeZoneDisplacementOutOfRange));
    /// ```
    pub fn parse(text: &str) -> Result<Self, DateTimeError> {
        let fields = match read_date_time(text)? {
            Literal::Special(Special::Late) => return Ok(Self::INFINITY),
            Literal::Special(Special::Early) => return Ok(Self::NEG_INFINITY),
            Literal::Special(Special::Epoch) => return Ok(Self(EPOCH_MICROS)),
            Literal::Fields(fields) => fields,
        };
        let offset = fields.offset.ok_or(DateTimeError::NeedsTimeZone)?;
        tm2timestamp(fields.year, fields.month, fields.day, fields.clock, offset)
            .map(Self)
            .ok_or(DateTimeError::TimestampOutOfRange)
    }
}

impl PgTime {
    /// PostgreSQL's `time_in`, reading `HH:MM[:SS[.f]]` up to 24:00:00 and `allballs`, with an
    /// offset read, checked and ignored, and the fraction rounded to the microsecond.
    ///
    /// # Errors
    ///
    /// The [`DateTimeError`] PostgreSQL raises, or [`DateTimeError::Unsupported`] for another
    /// spelling.
    ///
    /// ```
    /// use sql_values::{DateTimeError, PgTime};
    ///
    /// let read = |text| PgTime::parse(text).map(|t| t.to_string());
    /// assert_eq!(read("23:59:60").unwrap(), "24:00:00");
    /// assert_eq!(read("12:00:00.1234565").unwrap(), "12:00:00.123456");
    /// assert_eq!(read("24:00:00.000001"), Err(DateTimeError::FieldValueOutOfRange));
    /// ```
    pub fn parse(text: &str) -> Result<Self, DateTimeError> {
        Ok(Self(read_time(text)?.map_or(0, |(clock, _)| clock)))
    }
}

impl PgTimeTz {
    /// PostgreSQL's `timetz_in`, reading `HH:MM[:SS[.f]]` up to 24:00:00 with an offset, and
    /// `allballs`. A time without an offset takes the session's offset on the current date,
    /// which this reader does not model.
    ///
    /// # Errors
    ///
    /// The [`DateTimeError`] PostgreSQL raises, or [`DateTimeError::Unsupported`] for another
    /// spelling or a time without an offset.
    ///
    /// ```
    /// use sql_values::PgTimeTz;
    ///
    /// let read = |text| PgTimeTz::parse(text).map(|t| t.to_string());
    /// assert_eq!(read("12:00:00-05:30").unwrap(), "12:00:00-05:30");
    /// assert_eq!(read("allballs").unwrap(), "00:00:00+00");
    /// ```
    pub fn parse(text: &str) -> Result<Self, DateTimeError> {
        match read_time(text)? {
            None => Ok(Self { time: 0, offset: 0 }),
            Some((clock, Some(offset))) => Ok(Self {
                time: clock,
                offset,
            }),
            Some((_, None)) => Err(DateTimeError::Unsupported),
        }
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
    let (year_digits, rest) = bytes.split_at(year_len);
    let year = strtoint(year_digits)?;
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
            (fractional_second(&rest[..=len]), &fraction[len..])
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
