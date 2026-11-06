//! PostgreSQL 18's `date_in`, `time_in`, `timetz_in`, `timestamp_in` and `timestamptz_in` for the
//! ISO spelling, and the readers of the text PostgreSQL, MySQL and SQLite print.

use alloc::{string::String, vec::Vec};

use super::{
    DateTimeError, POSTGRES_EPOCH_JDATE, PgDate, PgTime, PgTimeTz, PgTimestamp, PgTimestampTz,
    USECS_PER_DAY, USECS_PER_HOUR, USECS_PER_MINUTE, USECS_PER_SEC, abbreviation_offset, date2j,
    days_in_month, is_valid_julian, local_offset, tm2timestamp,
};
use super::{
    decode::{
        DateOrder, DateType, Decoded, Dterr, WordType, ZoneSource, decode_date_time, decode_word,
    },
    tokens::{ADBC, parse_date_time},
};
use crate::{
    PgTimeZone, PgType, TimeZoneAbbreviations,
    cast::{is_pg_space, trim_pg_space},
    float::{round_ties_even, truncate_to_i64},
    zone::DEFAULT_ABBREVIATIONS,
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

    /// The context with `zone` as the session's `TimeZone`, which reads a `timestamptz` written
    /// without an offset, and whose own abbreviations a literal may write before those of the
    /// set.
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
    /// `yesterday` read, in the context's time zone.
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

/// A numeric offset as written, its sign and the digits and colons after it, before
/// `DecodeTimezone` checks it.
struct RawOffset<'a> {
    negative: bool,
    digits: &'a [u8],
}

/// What a word of a time literal reads as, by `DecodeTimezoneAbbrev`, `DecodeSpecial` and
/// `pg_tzset` in that order.
enum Word {
    /// `ad`, `false`, or `bc`, `true`.
    Era(bool),
    /// A zone written after the clock.
    Zone(LiteralZone),
}

/// A zone a time literal writes, by abbreviation or by name.
enum LiteralZone {
    /// `TZ` or `DTZ`, a fixed offset east of UTC in seconds.
    Fixed(i32),
    /// `DYNTZ`, an abbreviation whose meaning the date decides.
    Dynamic,
    /// A full zone name, `pg_tzset`.
    Named(PgTimeZone),
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
            Some(sign @ (b'+' | b'-')) if self.peek_at(1).is_some_and(|b| b.is_ascii_digit()) => {
                self.at += 1;
                let start = self.at;
                while self.peek().is_some_and(|b| b.is_ascii_digit() || b == b':') {
                    self.at += 1;
                }
                if matches!(self.peek(), Some(b'.' | b'-')) {
                    return None;
                }
                Some(RawOffset {
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

    /// The words that end a literal, each a run of letters `ParseDateTime` keeps as one token,
    /// lowercased, or `None` when something else is left.
    fn words(&mut self) -> Option<Vec<Vec<u8>>> {
        let mut words = Vec::new();
        loop {
            self.spaces();
            if self.is_done() {
                return Some(words);
            }
            let start = self.at;
            while self.peek().is_some_and(|b| b.is_ascii_alphabetic()) {
                self.at += 1;
            }
            if self.at == start || !(self.is_done() || self.peek().is_some_and(is_pg_space)) {
                return None;
            }
            words.push(self.bytes[start..self.at].to_ascii_lowercase());
        }
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
    let RawOffset { negative, digits } = *offset;
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

/// `DecodeTimezoneAbbrev`, then `DecodeSpecial`, then `pg_tzset`, for a lowercased word of a
/// time literal. Of `datetktbl`'s words only `ad` and `bc` are read here.
fn classify(word: &[u8], context: InputContext<'_>) -> Result<Word, DateTimeError> {
    let unsupported = DateTimeError::Unsupported;
    Ok(
        // A set's zone that does not exist is the one refusal `decode_word` answers.
        match decode_word(word, context).map_err(|_| DateTimeError::TimeZoneNotRecognized)? {
            WordType::Fixed { west, .. } => Word::Zone(LiteralZone::Fixed(-west)),
            WordType::Dynamic { .. } => Word::Zone(LiteralZone::Dynamic),
            WordType::Table(ADBC, value) => Word::Era(value == 1),
            WordType::Table(..) => return Err(unsupported),
            WordType::Unknown => Word::Zone(LiteralZone::Named(
                PgTimeZone::load(&String::from_utf8_lossy(word)).ok_or(unsupported)?,
            )),
        },
    )
}

/// The era and zone a literal's words write, refusing a second of either, or a zone beside a
/// numeric offset, which PostgreSQL reads as a syntax error.
fn classify_words(
    words: &[Vec<u8>],
    numeric_offset: bool,
    context: InputContext<'_>,
) -> Result<(Option<bool>, Option<LiteralZone>), DateTimeError> {
    let (mut era, mut zone) = (None, None);
    for word in words {
        match classify(word, context)? {
            Word::Era(_) if era.is_some() => return Err(DateTimeError::Unsupported),
            Word::Era(bc) => era = Some(bc),
            Word::Zone(_) if zone.is_some() || numeric_offset => {
                return Err(DateTimeError::Unsupported);
            }
            Word::Zone(written) => zone = Some(written),
        }
    }
    Ok((era, zone))
}

/// `MAXDATELEN`, whose `workbuf` of one byte more bounds a literal's tokens.
const MAX_DATE_LEN: usize = 128;

/// `ParseDateTime` and `DecodeDateTime` over a literal of `pg_type`, which a NUL ends as it ends
/// a C string.
fn decode<'a>(
    text: &str,
    context: InputContext<'a>,
    pg_type: PgType,
) -> Result<Decoded<'a>, DateTimeError> {
    let text = text.split('\0').next().unwrap_or_default();
    let tokens = parse_date_time(text.as_bytes(), MAX_DATE_LEN + 1)
        .map_err(|_| DateTimeError::InvalidInputSyntax(pg_type))?;
    let decoded = match decode_date_time(&tokens, context) {
        // Without an order, a literal the order touches reads only if every order reads it
        // alike, as `January 8, 1999` does.
        Err(Dterr::Other(DateTimeError::NeedsDateOrder)) => {
            let [mdy, dmy, ymd] = [DateOrder::Mdy, DateOrder::Dmy, DateOrder::Ymd]
                .map(|order| decode_date_time(&tokens, context.with_date_order(order)));
            if mdy == dmy && dmy == ymd {
                mdy
            } else {
                Err(Dterr::Other(DateTimeError::NeedsDateOrder))
            }
        }
        other => other,
    };
    decoded.map_err(|error| error.for_type(pg_type))
}

/// The offset a time literal writes, as `DecodeTimeOnly` settles it without a date.
enum TimeOffset {
    /// No offset, which the session's zone then gives.
    Unwritten,
    /// An offset east of UTC in seconds.
    Known(i32),
    /// A zone whose offset depends on the date, which the current date then decides.
    NeedsDate,
}

/// `DecodeTimeOnly` for the spellings the readers take, `allballs` and `HH:MM[:SS[.f]]` with
/// an optional offset, followed by a zone abbreviation or name, between blanks. `None` for
/// `allballs`.
fn read_time(
    text: &str,
    context: InputContext<'_>,
) -> Result<Option<(i64, TimeOffset)>, DateTimeError> {
    let bytes = trim_pg_space(text.as_bytes());
    if bytes.len() > MAX_LITERAL_LEN {
        return Err(DateTimeError::Unsupported);
    }
    if bytes.eq_ignore_ascii_case(b"allballs") {
        return Ok(None);
    }
    let mut scanner = Scanner::new(bytes);
    let raw = scanner.clock().ok_or(DateTimeError::Unsupported)?;
    let words = scanner.words().ok_or(DateTimeError::Unsupported)?;
    let clock = raw.decode()?;
    let offset = raw.offset.as_ref().map(decode_timezone).transpose()?;
    let (era, zone) = classify_words(&words, offset.is_some(), context)?;
    if era.is_some() {
        return Err(DateTimeError::Unsupported);
    }
    let offset = match (offset, zone) {
        (Some(offset), _) | (None, Some(LiteralZone::Fixed(offset))) => TimeOffset::Known(offset),
        (None, Some(LiteralZone::Named(zone))) => zone
            .fixed_offset()
            .map_or(TimeOffset::NeedsDate, TimeOffset::Known),
        (None, Some(LiteralZone::Dynamic)) => TimeOffset::NeedsDate,
        (None, None) => TimeOffset::Unwritten,
    };
    Ok(Some((check_time_overflow(clock)?, offset)))
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
        let decoded = decode(text, context, PgType::Date)?;
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
        let decoded = decode(text, context, PgType::Timestamp)?;
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
        let decoded = decode(text, context, PgType::TimestampTz)?;
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
    /// PostgreSQL's `time_in`, reading `HH:MM[:SS[.f]]` up to 24:00:00 and `allballs`, with an
    /// offset read, checked and ignored, and the fraction rounded to the microsecond.
    ///
    /// # Errors
    ///
    /// The [`DateTimeError`] PostgreSQL raises, or [`DateTimeError::Unsupported`] for another
    /// spelling.
    ///
    /// ```
    /// use sql_values::{DateTimeError, InputContext, PgTime};
    ///
    /// let read = |text| PgTime::parse(text, InputContext::new()).map(|t| t.to_string());
    /// assert_eq!(read("23:59:60").unwrap(), "24:00:00");
    /// assert_eq!(read("12:00:00.1234565").unwrap(), "12:00:00.123456");
    /// assert_eq!(read("24:00:00.000001"), Err(DateTimeError::FieldValueOutOfRange));
    /// ```
    pub fn parse(text: &str, context: InputContext<'_>) -> Result<Self, DateTimeError> {
        Ok(Self(
            read_time(text, context)?.map_or(0, |(clock, _)| clock),
        ))
    }
}

impl PgTimeTz {
    /// PostgreSQL's `timetz_in`, reading `HH:MM[:SS[.f]]` up to 24:00:00 with an offset or a
    /// zone of one fixed offset, and `allballs`. A time without an offset, or with a zone whose
    /// offset changes, takes the offset of the current date, which this reader does not model.
    ///
    /// # Errors
    ///
    /// The [`DateTimeError`] PostgreSQL raises, or [`DateTimeError::Unsupported`] for another
    /// spelling or a time whose offset the current date decides.
    ///
    /// ```
    /// use sql_values::{InputContext, PgTimeTz};
    ///
    /// let read = |text| PgTimeTz::parse(text, InputContext::new()).map(|t| t.to_string());
    /// assert_eq!(read("12:00:00-05:30").unwrap(), "12:00:00-05:30");
    /// assert_eq!(read("allballs").unwrap(), "00:00:00+00");
    /// ```
    pub fn parse(text: &str, context: InputContext<'_>) -> Result<Self, DateTimeError> {
        match read_time(text, context)? {
            None => Ok(Self { time: 0, offset: 0 }),
            Some((clock, TimeOffset::Known(offset))) => Ok(Self {
                time: clock,
                offset,
            }),
            Some((_, TimeOffset::Unwritten | TimeOffset::NeedsDate)) => {
                Err(DateTimeError::Unsupported)
            }
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
