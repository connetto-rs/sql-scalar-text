//! PostgreSQL 18's `date`, `time`, `timetz`, `timestamp` and `timestamptz`, held as PostgreSQL
//! stores them, printed by their output functions under `DateStyle` `ISO`, and converted into one
//! another in a [`PgTimeZone`].

mod arithmetic;
mod decode;
mod input;
#[cfg(feature = "jiff")]
mod jiff_conversions;
pub(crate) mod tokens;

use core::{cmp::Ordering, fmt};

pub use decode::DateOrder;
pub use input::{InputContext, parse_date, parse_time, parse_timestamp, parse_timestamp_tz};

use crate::{PgTimeZone, PgType};

pub(crate) const USECS_PER_SEC: i64 = 1_000_000;
pub(crate) const USECS_PER_MINUTE: i64 = 60_000_000;
pub(crate) const USECS_PER_HOUR: i64 = 3_600_000_000;
pub(crate) const USECS_PER_DAY: i64 = 86_400_000_000;
pub(crate) const MONTHS_PER_YEAR: i32 = 12;
/// `POSTGRES_EPOCH_JDATE`, the Julian day of 2000-01-01.
pub(crate) const POSTGRES_EPOCH_JDATE: i32 = 2_451_545;
/// Seconds from 1970-01-01, `UNIX_EPOCH_JDATE`, to 2000-01-01.
const UNIX_TO_POSTGRES_SECS: i64 = 946_684_800;
/// `DATE_END_JULIAN`, the Julian day of 5874898-01-01, past the last date.
const DATE_END_JULIAN: i32 = 2_147_483_494;
/// `TIMESTAMP_END_JULIAN - POSTGRES_EPOCH_JDATE`, the days from 2000-01-01 to 294277-01-01, the
/// first date past the last timestamp.
const TIMESTAMP_END_DAYS: i32 = 106_751_983;
/// `MIN_TIMESTAMP`, 4714-11-24 00:00:00 BC.
pub(crate) const MIN_TIMESTAMP: i64 = -211_813_488_000_000_000;
/// `END_TIMESTAMP`, 294277-01-01 00:00:00, past the last timestamp.
pub(crate) const END_TIMESTAMP: i64 = 9_223_371_331_200_000_000;
/// `TZDISP_LIMIT`, the largest offset a `timetz` holds, in seconds.
const TZDISP_LIMIT: i32 = 16 * 3_600;

/// Why PostgreSQL refuses a date or time, one variant per message it raises, or why this crate
/// cannot answer for it without more session state or a wider zone database.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, thiserror::Error)]
#[non_exhaustive]
pub enum DateTimeError {
    /// A year, month, day, hour, minute or second no calendar or clock has.
    #[error("date/time field value out of range")]
    FieldValueOutOfRange,
    /// An offset of 16 hours or more, or with minutes or seconds of 60 or more.
    #[error("time zone displacement out of range")]
    TimeZoneDisplacementOutOfRange,
    /// A date before 4714-11-24 BC or from 5874898-01-01.
    #[error("date out of range")]
    DateOutOfRange,
    /// A timestamp before 4714-11-24 BC or from 294277-01-01.
    #[error("timestamp out of range")]
    TimestampOutOfRange,
    /// A date from 294277-01-01 converted to `timestamp`.
    #[error("date out of range for timestamp")]
    DateOutOfRangeForTimestamp,
    /// `date - date` with an infinite date.
    #[error("cannot subtract infinite dates")]
    CannotSubtractInfiniteDates,
    /// A value outside jiff's years -9999 to 9999, an infinity included, where jiff's types or
    /// its time zone database are needed.
    #[error("value outside jiff's years -9999 to 9999")]
    OutsideJiffRange,
    /// A literal without an offset read as a `timestamptz` with no time zone in the
    /// [`InputContext`].
    #[error("the literal needs a time zone to read as timestamp with time zone")]
    NeedsTimeZone,
    /// A zone name, or an abbreviation whose set names a zone, that does not exist.
    #[error("time zone not recognized")]
    TimeZoneNotRecognized,
    /// Text the type's input function cannot read.
    #[error("invalid input syntax for type {}", .0.input_name())]
    InvalidInputSyntax(PgType),
    /// A date whose reading the `DateStyle` field order decides, read with no order in the
    /// [`InputContext`].
    #[error("the literal needs a DateStyle field order")]
    NeedsDateOrder,
    /// `now`, `today`, `tomorrow` or `yesterday` read with no current time in the
    /// [`InputContext`].
    #[error("the literal needs the current time")]
    NeedsCurrentTime,
}

/// A PostgreSQL `date`, days from 2000-01-01, from 4714-11-24 BC to 5874897-12-31 and the two
/// infinities, ordered as PostgreSQL orders it.
///
/// ```
/// use sql_values::{InputContext, PgDate};
///
/// let leap = PgDate::from_ymd(2024, 2, 29).unwrap();
/// assert_eq!(leap.to_string(), "2024-02-29");
/// assert_eq!(PgDate::parse("2024-02-29", InputContext::new()), Ok(leap));
/// assert_eq!(PgDate::from_ymd(-4713, 11, 24).unwrap().to_string(), "4714-11-24 BC");
/// assert!(PgDate::from_ymd(-4713, 11, 23).is_none());
/// assert!(PgDate::NEG_INFINITY < leap && leap < PgDate::INFINITY);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(try_from = "i32", into = "i32")
)]
pub struct PgDate(i32);

/// A PostgreSQL `time`, microseconds from midnight to 24:00:00 inclusive.
///
/// ```
/// use sql_values::PgTime;
///
/// let noon = PgTime::from_hms_micro(12, 0, 0, 500_000).unwrap();
/// assert_eq!(noon.to_string(), "12:00:00.5");
/// assert_eq!(PgTime::from_micros(86_400_000_000).unwrap().to_string(), "24:00:00");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(try_from = "i64", into = "i64")
)]
pub struct PgTime(i64);

/// A PostgreSQL `timetz`, a time of day with the offset east of UTC it was written with,
/// ordered by the instant and then by the offset, as `timetz_cmp` orders it.
///
/// ```
/// use sql_values::{PgTime, PgTimeTz};
///
/// let noon = PgTime::from_hms_micro(12, 0, 0, 0).unwrap();
/// let rome = PgTimeTz::new(noon, 3_600).unwrap();
/// assert_eq!(rome.to_string(), "12:00:00+01");
/// let utc = PgTimeTz::new(PgTime::from_hms_micro(11, 0, 0, 0).unwrap(), 0).unwrap();
/// // At one instant the offset decides, by PostgreSQL's zone in seconds west of UTC.
/// assert_ne!(rome, utc);
/// assert!(rome < utc);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(try_from = "(i64, i32)", into = "(i64, i32)"))]
pub struct PgTimeTz {
    time: i64,
    offset: i32,
}

/// A PostgreSQL `timestamp`, microseconds from 2000-01-01 00:00:00, from 4714-11-24 BC to
/// 294276-12-31 23:59:59.999999 and the two infinities.
///
/// ```
/// use sql_values::{InputContext, PgTimestamp};
///
/// let last = PgTimestamp::parse("294276-12-31 23:59:59.999999", InputContext::new()).unwrap();
/// assert_eq!(last.to_string(), "294276-12-31 23:59:59.999999");
/// assert_eq!(PgTimestamp::INFINITY.to_string(), "infinity");
/// assert!(last < PgTimestamp::INFINITY);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(try_from = "i64", into = "i64")
)]
pub struct PgTimestamp(i64);

/// A PostgreSQL `timestamptz`, the instant as microseconds from 2000-01-01 00:00:00 UTC, over the
/// range of [`PgTimestamp`], printed as under `TimeZone` `UTC`.
///
/// ```
/// use sql_values::{InputContext, PgTimestampTz};
///
/// let at = PgTimestampTz::parse("2026-03-29 12:00:00+02", InputContext::new()).unwrap();
/// assert_eq!(at.to_string(), "2026-03-29 10:00:00+00");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(try_from = "i64", into = "i64")
)]
pub struct PgTimestampTz(i64);

impl PgDate {
    /// `infinity`, PostgreSQL's `DATEVAL_NOEND`, after every date.
    pub const INFINITY: Self = Self(i32::MAX);
    /// `-infinity`, PostgreSQL's `DATEVAL_NOBEGIN`, before every date.
    pub const NEG_INFINITY: Self = Self(i32::MIN);

    /// The date of the proleptic Gregorian `year`, `month` and `day`, where year `Y` BC is year
    /// `1 - Y`, or `None` outside the calendar or PostgreSQL's dates.
    #[must_use]
    pub fn from_ymd(year: i32, month: u8, day: u8) -> Option<Self> {
        let (month, day) = (i32::from(month), i32::from(day));
        if !(1..=12).contains(&month) || day < 1 || day > days_in_month(year, month) {
            return None;
        }
        if !is_valid_julian(year, month) {
            return None;
        }
        Self::from_pg_days(date2j(year, month, day) - POSTGRES_EPOCH_JDATE)
    }

    /// The date `days` after 2000-01-01, as PostgreSQL stores it, where `i32::MAX` and `i32::MIN`
    /// are the infinities, or `None` outside PostgreSQL's dates.
    #[must_use]
    pub const fn from_pg_days(days: i32) -> Option<Self> {
        if is_valid_date(days) || days == i32::MAX || days == i32::MIN {
            Some(Self(days))
        } else {
            None
        }
    }

    /// Days from 2000-01-01, as PostgreSQL stores the date.
    #[must_use]
    pub const fn pg_days(self) -> i32 {
        self.0
    }

    /// Whether the date is neither `infinity` nor `-infinity`.
    #[must_use]
    pub const fn is_finite(self) -> bool {
        self.0 != i32::MAX && self.0 != i32::MIN
    }

    /// The year, month and day, where year `Y` BC is year `1 - Y`, or `None` for an infinity.
    ///
    /// ```
    /// use sql_values::PgDate;
    ///
    /// assert_eq!(PgDate::from_pg_days(0).unwrap().ymd(), Some((2000, 1, 1)));
    /// assert_eq!(PgDate::INFINITY.ymd(), None);
    /// ```
    #[must_use]
    pub fn ymd(self) -> Option<(i32, u8, u8)> {
        self.is_finite().then(|| {
            let (year, month, day) = j2date(self.0 + POSTGRES_EPOCH_JDATE);
            (year, narrow_u8(month.into()), narrow_u8(day.into()))
        })
    }
}

impl PgTime {
    /// Midnight, `00:00:00`.
    pub const MIDNIGHT: Self = Self(0);

    /// The time `micros` microseconds after midnight, or `None` outside 00:00:00 to 24:00:00.
    #[must_use]
    pub const fn from_micros(micros: i64) -> Option<Self> {
        if 0 <= micros && micros <= USECS_PER_DAY {
            Some(Self(micros))
        } else {
            None
        }
    }

    /// The time of `hour`, `minute`, `second` and `microsecond`, or `None` past 24:00:00 or
    /// with a field beyond its clock.
    #[must_use]
    pub fn from_hms_micro(hour: u8, minute: u8, second: u8, microsecond: u32) -> Option<Self> {
        if hour > 24 || minute > 59 || second > 59 || microsecond > 999_999 {
            return None;
        }
        Self::from_micros(
            i64::from(hour) * USECS_PER_HOUR
                + i64::from(minute) * USECS_PER_MINUTE
                + i64::from(second) * USECS_PER_SEC
                + i64::from(microsecond),
        )
    }

    /// Microseconds after midnight, as PostgreSQL stores the time.
    #[must_use]
    pub const fn micros(self) -> i64 {
        self.0
    }

    /// The hour, minute, second and microsecond, where 24:00:00 is hour 24.
    ///
    /// ```
    /// use sql_values::PgTime;
    ///
    /// let t = PgTime::from_micros(45_296_000_007).unwrap();
    /// assert_eq!(t.hms_micro(), (12, 34, 56, 7));
    /// ```
    #[must_use]
    pub fn hms_micro(self) -> (u8, u8, u8, u32) {
        let (hour, minute, second, micro) = split_clock(self.0);
        (
            narrow_u8(hour),
            narrow_u8(minute),
            narrow_u8(second),
            u32::try_from(micro).expect("a microsecond of a second fits u32"),
        )
    }
}

impl PgTimeTz {
    /// The time `time` at `offset` seconds east of UTC, or `None` beyond PostgreSQL's 16 hours.
    #[must_use]
    pub const fn new(time: PgTime, offset: i32) -> Option<Self> {
        if -TZDISP_LIMIT <= offset && offset <= TZDISP_LIMIT {
            Some(Self {
                time: time.0,
                offset,
            })
        } else {
            None
        }
    }

    /// The time of day as written.
    #[must_use]
    pub const fn time(self) -> PgTime {
        PgTime(self.time)
    }

    /// The offset east of UTC, in seconds.
    #[must_use]
    pub const fn offset(self) -> i32 {
        self.offset
    }

    /// `timetz_cmp_internal`'s keys, the time at UTC and then PostgreSQL's zone, seconds west.
    fn sort_key(self) -> (i64, i32) {
        (
            self.time - i64::from(self.offset) * USECS_PER_SEC,
            -self.offset,
        )
    }
}

impl PartialOrd for PgTimeTz {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PgTimeTz {
    fn cmp(&self, other: &Self) -> Ordering {
        self.sort_key().cmp(&other.sort_key())
    }
}

impl PgTimestamp {
    /// `infinity`, PostgreSQL's `DT_NOEND`, after every timestamp.
    pub const INFINITY: Self = Self(i64::MAX);
    /// `-infinity`, PostgreSQL's `DT_NOBEGIN`, before every timestamp.
    pub const NEG_INFINITY: Self = Self(i64::MIN);

    /// The timestamp `micros` microseconds after 2000-01-01 00:00:00, as PostgreSQL stores it,
    /// where `i64::MAX` and `i64::MIN` are the infinities, or `None` outside PostgreSQL's range.
    #[must_use]
    pub const fn from_pg_micros(micros: i64) -> Option<Self> {
        if is_valid_timestamp(micros) || micros == i64::MAX || micros == i64::MIN {
            Some(Self(micros))
        } else {
            None
        }
    }

    /// The timestamp of `time` on a finite `date`, where 24:00:00 is midnight of the next day,
    /// or `None` for an infinite date or outside PostgreSQL's range.
    ///
    /// ```
    /// use sql_values::{PgDate, PgTime, PgTimestamp};
    ///
    /// let date = PgDate::from_ymd(2024, 12, 31).unwrap();
    /// let at = PgTimestamp::new(date, PgTime::from_micros(86_400_000_000).unwrap()).unwrap();
    /// assert_eq!(at.to_string(), "2025-01-01 00:00:00");
    /// ```
    #[must_use]
    pub fn new(date: PgDate, time: PgTime) -> Option<Self> {
        if !date.is_finite() {
            return None;
        }
        i64::from(date.0)
            .checked_mul(USECS_PER_DAY)
            .and_then(|micros| micros.checked_add(time.0))
            .and_then(Self::from_pg_micros)
            .filter(|at| at.is_finite())
    }

    /// The timestamp PostgreSQL's arithmetic leaves in `micros`, unchecked as PostgreSQL leaves
    /// it, as `AdjustTimestampForTypmod` rounds 294276-12-31 23:59:59.5 up to 294277-01-01.
    pub(crate) const fn from_stored(micros: i64) -> Self {
        Self(micros)
    }

    /// Microseconds from 2000-01-01 00:00:00, as PostgreSQL stores the timestamp.
    #[must_use]
    pub const fn pg_micros(self) -> i64 {
        self.0
    }

    /// Whether the timestamp is neither `infinity` nor `-infinity`.
    #[must_use]
    pub const fn is_finite(self) -> bool {
        self.0 != i64::MAX && self.0 != i64::MIN
    }

    /// The date, infinite for an infinite timestamp, as `timestamp_date` casts it.
    #[must_use]
    pub fn date(self) -> PgDate {
        match self.0 {
            i64::MAX => PgDate::INFINITY,
            i64::MIN => PgDate::NEG_INFINITY,
            micros => PgDate(narrow_days(micros.div_euclid(USECS_PER_DAY))),
        }
    }

    /// The time of day, or `None` for an infinity.
    #[must_use]
    pub const fn time(self) -> Option<PgTime> {
        if self.is_finite() {
            Some(PgTime(self.0.rem_euclid(USECS_PER_DAY)))
        } else {
            None
        }
    }

    /// PostgreSQL's `timestamp::timestamptz` with `zone` as the session's `TimeZone`, the local
    /// clock resolved by `DetermineTimeZoneOffset`. A clock the spring-forward gap skips takes
    /// the offset before the change, and a clock the fall-back repeats the offset after it.
    ///
    /// # Errors
    ///
    /// [`DateTimeError::TimestampOutOfRange`] where the instant leaves PostgreSQL's range, and
    /// [`DateTimeError::OutsideJiffRange`] where an IANA zone is asked about a clock outside
    /// jiff's years.
    ///
    /// ```
    /// use sql_values::{InputContext, PgTimeZone, PgTimestamp};
    ///
    /// let rome = PgTimeZone::parse("Europe/Rome").unwrap();
    /// let gap = PgTimestamp::parse("2026-03-29 02:30:00", InputContext::new()).unwrap();
    /// assert_eq!(gap.to_timestamptz(&rome).unwrap().to_string(), "2026-03-29 01:30:00+00");
    /// ```
    pub fn to_timestamptz(self, zone: &PgTimeZone) -> Result<PgTimestampTz, DateTimeError> {
        if !self.is_finite() {
            return Ok(PgTimestampTz(self.0));
        }
        let offset = local_offset(zone, self.0)?;
        let instant = self.0 - i64::from(offset) * USECS_PER_SEC;
        if is_valid_timestamp(instant) {
            Ok(PgTimestampTz(instant))
        } else {
            Err(DateTimeError::TimestampOutOfRange)
        }
    }
}

impl PgTimestampTz {
    /// `infinity`, PostgreSQL's `DT_NOEND`, after every instant.
    pub const INFINITY: Self = Self(i64::MAX);
    /// `-infinity`, PostgreSQL's `DT_NOBEGIN`, before every instant.
    pub const NEG_INFINITY: Self = Self(i64::MIN);

    /// The instant `micros` microseconds after 2000-01-01 00:00:00 UTC, as PostgreSQL stores it,
    /// where `i64::MAX` and `i64::MIN` are the infinities, or `None` outside PostgreSQL's range.
    #[must_use]
    pub const fn from_pg_micros(micros: i64) -> Option<Self> {
        match PgTimestamp::from_pg_micros(micros) {
            Some(at) => Some(Self(at.0)),
            None => None,
        }
    }

    /// The instant PostgreSQL's arithmetic leaves in `micros`, unchecked as PostgreSQL leaves
    /// it.
    pub(crate) const fn from_stored(micros: i64) -> Self {
        Self(micros)
    }

    /// Microseconds from 2000-01-01 00:00:00 UTC, as PostgreSQL stores the instant.
    #[must_use]
    pub const fn pg_micros(self) -> i64 {
        self.0
    }

    /// Whether the instant is neither `infinity` nor `-infinity`.
    #[must_use]
    pub const fn is_finite(self) -> bool {
        self.0 != i64::MAX && self.0 != i64::MIN
    }

    /// PostgreSQL's `timestamptz::timestamp` with `zone` as the session's `TimeZone`, the local
    /// clock of the instant.
    ///
    /// # Errors
    ///
    /// [`DateTimeError::TimestampOutOfRange`] where the local clock leaves PostgreSQL's range,
    /// and [`DateTimeError::OutsideJiffRange`] where an IANA zone is asked about an instant
    /// outside jiff's years.
    ///
    /// ```
    /// use sql_values::{InputContext, PgTimeZone, PgTimestampTz};
    ///
    /// let sydney = PgTimeZone::parse("Australia/Sydney").unwrap();
    /// let at = PgTimestampTz::parse("2026-01-01 00:00:00+00", InputContext::new()).unwrap();
    /// assert_eq!(at.to_timestamp(&sydney).unwrap().to_string(), "2026-01-01 11:00:00");
    /// ```
    pub fn to_timestamp(self, zone: &PgTimeZone) -> Result<PgTimestamp, DateTimeError> {
        if !self.is_finite() {
            return Ok(PgTimestamp(self.0));
        }
        let local = self.0 + i64::from(utc_offset(zone, self.0)?) * USECS_PER_SEC;
        if is_valid_timestamp(local) {
            Ok(PgTimestamp(local))
        } else {
            Err(DateTimeError::TimestampOutOfRange)
        }
    }

    /// PostgreSQL's `timestamptz::date` with `zone` as the session's `TimeZone`, the local date
    /// of the instant, which PostgreSQL does not range-check.
    ///
    /// # Errors
    ///
    /// [`DateTimeError::OutsideJiffRange`] where an IANA zone is asked about an instant outside
    /// jiff's years.
    pub fn to_date(self, zone: &PgTimeZone) -> Result<PgDate, DateTimeError> {
        match self.0 {
            i64::MAX => Ok(PgDate::INFINITY),
            i64::MIN => Ok(PgDate::NEG_INFINITY),
            utc => {
                let local = utc + i64::from(utc_offset(zone, utc)?) * USECS_PER_SEC;
                Ok(PgDate(narrow_days(local.div_euclid(USECS_PER_DAY))))
            }
        }
    }
}

/// The offset `zone` has at the instant `utc`, microseconds from 2000-01-01 UTC, as
/// `timestamp2tm` asks `pg_localtime` for it.
pub(crate) fn utc_offset(zone: &PgTimeZone, utc: i64) -> Result<i32, DateTimeError> {
    let unix = utc.div_euclid(USECS_PER_SEC) + UNIX_TO_POSTGRES_SECS;
    zone.utc_offset(unix).ok_or(DateTimeError::OutsideJiffRange)
}

/// The offset `DetermineTimeZoneOffset` gives the local clock `local`, microseconds from
/// 2000-01-01, in `zone`. A clock jiff cannot place that lies further past PostgreSQL's range
/// than any of jiff's offsets reaches is [`DateTimeError::TimestampOutOfRange`], as PostgreSQL
/// finds it after the offset.
pub(crate) fn local_offset(zone: &PgTimeZone, local: i64) -> Result<i32, DateTimeError> {
    /// jiff's offsets lie within 25:59:59 of UTC.
    const JIFF_OFFSET_LIMIT: i64 = 26 * USECS_PER_HOUR;
    let unix = local.div_euclid(USECS_PER_SEC) + UNIX_TO_POSTGRES_SECS;
    zone.local_offset(unix).ok_or(
        if (MIN_TIMESTAMP - JIFF_OFFSET_LIMIT..END_TIMESTAMP + JIFF_OFFSET_LIMIT).contains(&local) {
            DateTimeError::OutsideJiffRange
        } else {
            DateTimeError::TimestampOutOfRange
        },
    )
}

/// `DetermineTimeZoneAbbrevOffset`, the offset the uppercased `abbreviation` had in `zone` at the
/// local clock `local`, microseconds from 2000-01-01, or the zone's own offset there when the
/// zone never used it.
pub(crate) fn abbreviation_offset(
    zone: &PgTimeZone,
    abbreviation: &str,
    local: i64,
) -> Result<i32, DateTimeError> {
    let zone_offset = local_offset(zone, local)?;
    let unix = local.div_euclid(USECS_PER_SEC) + UNIX_TO_POSTGRES_SECS - i64::from(zone_offset);
    Ok(zone
        .interpret_abbreviation(abbreviation, unix)?
        .map_or(zone_offset, |meaning| meaning.offset))
}

/// `IS_VALID_TIMESTAMP`.
pub(crate) const fn is_valid_timestamp(micros: i64) -> bool {
    MIN_TIMESTAMP <= micros && micros < END_TIMESTAMP
}

/// `IS_VALID_DATE`.
const fn is_valid_date(days: i32) -> bool {
    -POSTGRES_EPOCH_JDATE <= days && days < DATE_END_JULIAN - POSTGRES_EPOCH_JDATE
}

/// `IS_VALID_JULIAN`, which checks the lower bound only to the month.
pub(crate) const fn is_valid_julian(year: i32, month: i32) -> bool {
    (year > -4713 || (year == -4713 && month >= 11))
        && (year < 5_874_898 || (year == 5_874_898 && month < 6))
}

/// `tm2timestamp`, the timestamp of a date within `IS_VALID_JULIAN` and a clock up to
/// 24:00:00, with `offset` seconds east subtracted, or `None` outside PostgreSQL's range.
pub(crate) fn tm2timestamp(
    year: i32,
    month: i32,
    day: i32,
    clock: i64,
    offset: i32,
) -> Option<i64> {
    if !is_valid_julian(year, month) {
        return None;
    }
    let days = i64::from(date2j(year, month, day) - POSTGRES_EPOCH_JDATE);
    days.checked_mul(USECS_PER_DAY)?
        .checked_add(clock)?
        .checked_sub(i64::from(offset) * USECS_PER_SEC)
        .filter(|&micros| is_valid_timestamp(micros))
}

/// `timestamp2tm` without a zone, the year, month, day and clock of a timestamp whose Julian
/// day is at least -1.
pub(crate) fn timestamp2tm(micros: i64) -> (i32, i32, i32, i64) {
    let (year, month, day) =
        j2date(narrow_days(micros.div_euclid(USECS_PER_DAY)) + POSTGRES_EPOCH_JDATE);
    (year, month, day, micros.rem_euclid(USECS_PER_DAY))
}

/// `date2j`, the Julian day of a proleptic Gregorian date, wrapping past `int` as PostgreSQL's
/// `-fwrapv` build does for a year far outside `IS_VALID_JULIAN`.
pub(crate) const fn date2j(year: i32, month: i32, day: i32) -> i32 {
    let (year, month) = if month > 2 {
        (year.wrapping_add(4800), month + 1)
    } else {
        (year.wrapping_add(4799), month + 13)
    };
    let century = year / 100;
    let mut julian = year.wrapping_mul(365).wrapping_sub(32_167);
    julian = julian.wrapping_add(year / 4 - century + century / 4);
    julian.wrapping_add(7_834 * month / 256).wrapping_add(day)
}

/// `j2date`, the proleptic Gregorian date of a Julian day, in C's unsigned arithmetic that
/// wraps for a day before -32044.
pub(crate) const fn j2date(julian_day: i32) -> (i32, i32, i32) {
    let mut julian = julian_day.cast_unsigned().wrapping_add(32_044);
    let mut quad = julian / 146_097;
    let extra = (julian - quad * 146_097) * 4 + 3;
    julian = julian.wrapping_add(60 + quad * 3 + extra / 146_097);
    quad = julian / 1_461;
    julian -= quad * 1_461;
    let mut year = julian * 4 / 1_461;
    julian = if year == 0 {
        (julian + 306) % 366
    } else {
        (julian + 305) % 365
    } + 123;
    year += quad * 4;
    let quad = julian * 2_141 / 65_536;
    let day = julian - 7_834 * quad / 256;
    let month = (quad + 10) % 12 + 1;
    // The year is below 2^31 + 4800 and the day and month below 32.
    (
        year.cast_signed() - 4_800,
        month.cast_signed(),
        day.cast_signed(),
    )
}

/// Days in `month` of the proleptic Gregorian `year`, `0` being 1 BC.
pub(crate) const fn days_in_month(year: i32, month: i32) -> i32 {
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days from 2000-01-01 of a value PostgreSQL holds, which always fit `int`.
fn narrow_days(days: i64) -> i32 {
    i32::try_from(days).expect("a timestamp lies under 2^31 days from 2000-01-01")
}

/// A month, day or clock field, which always fits `u8`.
fn narrow_u8(field: i64) -> u8 {
    u8::try_from(field).expect("a calendar or clock field fits u8")
}

/// The hour, minute, second and microsecond of a clock up to 24:00:00.
const fn split_clock(micros: i64) -> (i64, i64, i64, i64) {
    (
        micros / USECS_PER_HOUR,
        micros % USECS_PER_HOUR / USECS_PER_MINUTE,
        micros % USECS_PER_MINUTE / USECS_PER_SEC,
        micros % USECS_PER_SEC,
    )
}

/// `EncodeDateOnly` under `DateStyle` `ISO`, `Y BC` for year `1 - Y`, with the era left to the
/// caller.
fn write_ymd(f: &mut fmt::Formatter<'_>, year: i32, month: i32, day: i32) -> fmt::Result {
    let shown = if year > 0 { year } else { 1 - year };
    write!(f, "{shown:04}-{month:02}-{day:02}")
}

/// `HH:MM:SS` and `AppendSeconds`' fraction without trailing zeros.
fn write_clock(f: &mut fmt::Formatter<'_>, micros: i64) -> fmt::Result {
    let (hour, minute, second, mut fraction) = split_clock(micros);
    write!(f, "{hour:02}:{minute:02}:{second:02}")?;
    if fraction > 0 {
        let mut width = 6;
        while fraction % 10 == 0 {
            fraction /= 10;
            width -= 1;
        }
        write!(f, ".{fraction:0width$}")?;
    }
    Ok(())
}

/// `EncodeTimezone`, `±HH`, `±HH:MM` or `±HH:MM:SS` of an offset east of UTC.
fn write_offset(f: &mut fmt::Formatter<'_>, offset: i32) -> fmt::Result {
    let sign = if offset >= 0 { '+' } else { '-' };
    let magnitude = offset.unsigned_abs();
    let (hour, minute, second) = (magnitude / 3_600, magnitude / 60 % 60, magnitude % 60);
    write!(f, "{sign}{hour:02}")?;
    if second != 0 {
        write!(f, ":{minute:02}:{second:02}")
    } else if minute != 0 {
        write!(f, ":{minute:02}")
    } else {
        Ok(())
    }
}

/// `EncodeDateTime` under `DateStyle` `ISO`, with `+00` for a `timestamptz` in `UTC`.
fn write_timestamp(f: &mut fmt::Formatter<'_>, micros: i64, zone: bool) -> fmt::Result {
    match micros {
        i64::MAX => return f.write_str("infinity"),
        i64::MIN => return f.write_str("-infinity"),
        _ => {}
    }
    let (year, month, day, clock) = timestamp2tm(micros);
    write_ymd(f, year, month, day)?;
    f.write_str(" ")?;
    write_clock(f, clock)?;
    if zone {
        f.write_str("+00")?;
    }
    if year <= 0 {
        f.write_str(" BC")?;
    }
    Ok(())
}

impl fmt::Display for PgDate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            i32::MAX => f.write_str("infinity"),
            i32::MIN => f.write_str("-infinity"),
            days => {
                let (year, month, day) = j2date(days + POSTGRES_EPOCH_JDATE);
                write_ymd(f, year, month, day)?;
                if year <= 0 {
                    f.write_str(" BC")?;
                }
                Ok(())
            }
        }
    }
}

impl fmt::Display for PgTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_clock(f, self.0)
    }
}

impl fmt::Display for PgTimeTz {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_clock(f, self.time)?;
        write_offset(f, self.offset)
    }
}

impl fmt::Display for PgTimestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_timestamp(f, self.0, false)
    }
}

impl fmt::Display for PgTimestampTz {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_timestamp(f, self.0, true)
    }
}

impl TryFrom<i32> for PgDate {
    type Error = DateTimeError;

    fn try_from(days: i32) -> Result<Self, Self::Error> {
        Self::from_pg_days(days).ok_or(DateTimeError::DateOutOfRange)
    }
}

impl From<PgDate> for i32 {
    fn from(date: PgDate) -> Self {
        date.0
    }
}

impl TryFrom<i64> for PgTime {
    type Error = DateTimeError;

    fn try_from(micros: i64) -> Result<Self, Self::Error> {
        Self::from_micros(micros).ok_or(DateTimeError::FieldValueOutOfRange)
    }
}

impl From<PgTime> for i64 {
    fn from(time: PgTime) -> Self {
        time.0
    }
}

impl TryFrom<(i64, i32)> for PgTimeTz {
    type Error = DateTimeError;

    fn try_from((micros, offset): (i64, i32)) -> Result<Self, Self::Error> {
        let time = PgTime::try_from(micros)?;
        Self::new(time, offset).ok_or(DateTimeError::TimeZoneDisplacementOutOfRange)
    }
}

impl From<PgTimeTz> for (i64, i32) {
    fn from(time: PgTimeTz) -> Self {
        (time.time, time.offset)
    }
}

impl TryFrom<i64> for PgTimestamp {
    type Error = DateTimeError;

    fn try_from(micros: i64) -> Result<Self, Self::Error> {
        Self::from_pg_micros(micros).ok_or(DateTimeError::TimestampOutOfRange)
    }
}

impl From<PgTimestamp> for i64 {
    fn from(at: PgTimestamp) -> Self {
        at.0
    }
}

impl TryFrom<i64> for PgTimestampTz {
    type Error = DateTimeError;

    fn try_from(micros: i64) -> Result<Self, Self::Error> {
        Self::from_pg_micros(micros).ok_or(DateTimeError::TimestampOutOfRange)
    }
}

impl From<PgTimestampTz> for i64 {
    fn from(at: PgTimestampTz) -> Self {
        at.0
    }
}
