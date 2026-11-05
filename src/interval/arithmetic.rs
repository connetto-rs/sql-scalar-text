//! PostgreSQL 18's arithmetic on intervals, ported from `timestamp.c` and `date.c` with
//! PostgreSQL's order of checks.

use core::cmp::Ordering;

use chrono::{Datelike, NaiveDate, NaiveDateTime, NaiveTime, Timelike};

use super::{
    IntervalError, PgInterval,
    float::{
        fits_in_i32, fits_in_i64, int_to_f64, round_ties_even, truncate_to_i32, truncate_to_i64,
        ts_round,
    },
};

const DAYS_PER_MONTH: f64 = 30.0;
const SECS_PER_DAY: f64 = 86_400.0;
const MONTHS_PER_YEAR: i32 = 12;
const USECS_PER_DAY: i64 = 86_400_000_000;
const USECS_PER_SEC: i64 = 1_000_000;

/// `POSTGRES_EPOCH_JDATE`, the Julian day of 2000-01-01.
const POSTGRES_EPOCH_JDATE: i64 = 2_451_545;
/// The Julian day of 0001-01-01 less one, as `chrono` counts that day as day 1 of the era.
const JULIAN_BEFORE_CE: i64 = 1_721_425;
/// `MIN_TIMESTAMP`, 4714-11-24 BC.
const MIN_TIMESTAMP: i64 = -211_813_488_000_000_000;
/// `END_TIMESTAMP`, 294277-01-01, past the last timestamp.
const END_TIMESTAMP: i64 = 9_223_371_331_200_000_000;

/// Why a timestamp or time shifted by an interval has no value here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, thiserror::Error)]
pub enum ShiftError {
    /// The timestamp lies outside PostgreSQL's, or the shift leaves them.
    #[error("timestamp out of range")]
    TimestampOutOfRange,
    /// The interval `timestamp - interval` negates has a field at its minimum.
    #[error("interval out of range")]
    IntervalOutOfRange,
    /// `time + interval` of an infinite interval.
    #[error("cannot add infinite interval to time")]
    InfiniteIntervalAddedToTime,
    /// `time - interval` of an infinite interval.
    #[error("cannot subtract infinite interval from time")]
    InfiniteIntervalSubtractedFromTime,
    /// PostgreSQL answers a timestamp `chrono` cannot hold, `infinity`, `-infinity` or one past
    /// `chrono`'s last year.
    #[error("timestamp beyond chrono")]
    Unrepresentable,
}

impl PgInterval {
    /// `Greater` for `infinity`, `Less` for `-infinity`, `None` when finite.
    const fn infinity(&self) -> Option<Ordering> {
        if self.fields_equal(&Self::INFINITY) {
            Some(Ordering::Greater)
        } else if self.fields_equal(&Self::NEG_INFINITY) {
            Some(Ordering::Less)
        } else {
            None
        }
    }

    /// `self` unless its fields spell an infinity, which a finite result may not.
    const fn finite(self) -> Result<Self, IntervalError> {
        if self.is_finite() {
            Ok(self)
        } else {
            Err(IntervalError::OutOfRange)
        }
    }

    /// The infinity on the side `sign` names.
    const fn infinite(sign: Ordering) -> Self {
        match sign {
            Ordering::Less => Self::NEG_INFINITY,
            _ => Self::INFINITY,
        }
    }

    /// PostgreSQL's `interval_pl`, `self + other`.
    ///
    /// # Errors
    ///
    /// [`IntervalError::OutOfRange`] where a field overflows, the sum spells an infinity, or
    /// opposite infinities meet.
    ///
    /// ```
    /// use sql_values::{IntervalError, PgInterval, parse_interval};
    ///
    /// let day = parse_interval("1 day").unwrap();
    /// let hour = parse_interval("-01:00:00").unwrap();
    /// assert_eq!(day.checked_add(hour).unwrap().to_string(), "1 day -01:00:00");
    /// assert_eq!(
    ///     PgInterval::INFINITY.checked_add(PgInterval::NEG_INFINITY),
    ///     Err(IntervalError::OutOfRange),
    /// );
    /// ```
    pub fn checked_add(self, other: Self) -> Result<Self, IntervalError> {
        match (self.infinity(), other.infinity()) {
            (Some(left), Some(right)) if left != right => Err(IntervalError::OutOfRange),
            (Some(_), _) => Ok(self),
            (None, Some(_)) => Ok(other),
            (None, None) => Self::finite_fields(FieldResults {
                months: self.months.checked_add(other.months),
                days: self.days.checked_add(other.days),
                microseconds: self.microseconds.checked_add(other.microseconds),
            }),
        }
    }

    /// PostgreSQL's `interval_mi`, `self - other`.
    ///
    /// # Errors
    ///
    /// [`IntervalError::OutOfRange`] where a field overflows, the difference spells an infinity,
    /// or like infinities meet.
    pub fn checked_sub(self, other: Self) -> Result<Self, IntervalError> {
        match (self.infinity(), other.infinity()) {
            (Some(left), Some(right)) if left == right => Err(IntervalError::OutOfRange),
            (Some(_), _) => Ok(self),
            (None, Some(right)) => Ok(Self::infinite(right.reverse())),
            (None, None) => Self::finite_fields(FieldResults {
                months: self.months.checked_sub(other.months),
                days: self.days.checked_sub(other.days),
                microseconds: self.microseconds.checked_sub(other.microseconds),
            }),
        }
    }

    /// PostgreSQL's `interval_um`, `-self`.
    ///
    /// # Errors
    ///
    /// [`IntervalError::OutOfRange`] where a field sits at its minimum, or the negation spells an
    /// infinity.
    ///
    /// ```
    /// use sql_values::{IntervalError, parse_interval};
    ///
    /// let span = parse_interval("1 mon -2 days").unwrap();
    /// assert_eq!(span.checked_neg().unwrap().to_string(), "-1 mons +2 days");
    /// let minimum = parse_interval("-2147483648 days").unwrap();
    /// assert_eq!(minimum.checked_neg(), Err(IntervalError::OutOfRange));
    /// ```
    pub fn checked_neg(self) -> Result<Self, IntervalError> {
        match self.infinity() {
            Some(sign) => Ok(Self::infinite(sign.reverse())),
            None => Self::finite_fields(FieldResults {
                months: self.months.checked_neg(),
                days: self.days.checked_neg(),
                microseconds: self.microseconds.checked_neg(),
            }),
        }
    }

    /// PostgreSQL's `interval_mul`, `self * factor`.
    ///
    /// Months and days scale apart, what a month leaves over cascades into days as 30 and what a
    /// day leaves over into the clock as 24 hours, each rounded to the microsecond as PostgreSQL
    /// rounds it.
    ///
    /// # Errors
    ///
    /// [`IntervalError::OutOfRange`] for a `NaN` factor, zero against an infinity, an infinite
    /// factor against a zero interval, and a product beyond the type.
    ///
    /// ```
    /// use sql_values::{IntervalError, PgInterval, parse_interval};
    ///
    /// let month = parse_interval("1 mon 1 day").unwrap();
    /// assert_eq!(month.checked_mul(1.5).unwrap().to_string(), "1 mon 16 days 12:00:00");
    /// assert_eq!(month.checked_mul(f64::INFINITY), Ok(PgInterval::INFINITY));
    /// assert_eq!(month.checked_mul(f64::NAN), Err(IntervalError::OutOfRange));
    /// ```
    pub fn checked_mul(self, factor: f64) -> Result<Self, IntervalError> {
        if factor.is_nan() {
            return Err(IntervalError::OutOfRange);
        }
        if let Some(sign) = self.infinity() {
            return match factor.partial_cmp(&0.0) {
                Some(Ordering::Less) => Ok(Self::infinite(sign.reverse())),
                Some(Ordering::Greater) => Ok(self),
                _ => Err(IntervalError::OutOfRange),
            };
        }
        if factor.is_infinite() {
            return match self.span_microseconds().cmp(&0) {
                Ordering::Equal => Err(IntervalError::OutOfRange),
                sign if factor < 0.0 => Ok(Self::infinite(sign.reverse())),
                sign => Ok(Self::infinite(sign)),
            };
        }
        self.cascade(|field| field * factor)
    }

    /// PostgreSQL's `interval_div`, `self / factor`, cascading as
    /// [`checked_mul`](Self::checked_mul) does.
    ///
    /// # Errors
    ///
    /// [`IntervalError::DivisionByZero`] for a zero factor, and [`IntervalError::OutOfRange`] for
    /// a `NaN` factor, an infinity divided by an infinity, and a quotient beyond the type.
    ///
    /// ```
    /// use sql_values::{IntervalError, parse_interval};
    ///
    /// let span = parse_interval("1 mon 1 day 00:00:01").unwrap();
    /// assert_eq!(span.checked_div(7.0).unwrap().to_string(), "4 days 10:17:08.6896");
    /// assert_eq!(span.checked_div(f64::INFINITY).unwrap().to_string(), "00:00:00");
    /// assert_eq!(span.checked_div(0.0), Err(IntervalError::DivisionByZero));
    /// ```
    pub fn checked_div(self, factor: f64) -> Result<Self, IntervalError> {
        if factor == 0.0 {
            return Err(IntervalError::DivisionByZero);
        }
        if factor.is_nan() {
            return Err(IntervalError::OutOfRange);
        }
        if let Some(sign) = self.infinity() {
            return if factor.is_infinite() {
                Err(IntervalError::OutOfRange)
            } else if factor < 0.0 {
                Ok(Self::infinite(sign.reverse()))
            } else {
                Ok(self)
            };
        }
        self.cascade(|field| field / factor)
    }

    /// The fields of a finite result, each `None` where it overflowed.
    fn finite_fields(fields: FieldResults) -> Result<Self, IntervalError> {
        Self {
            months: fields.months.ok_or(IntervalError::OutOfRange)?,
            days: fields.days.ok_or(IntervalError::OutOfRange)?,
            microseconds: fields.microseconds.ok_or(IntervalError::OutOfRange)?,
        }
        .finite()
    }

    /// The finite part of `interval_mul` and `interval_div`, `scale` being the multiplication or
    /// the division by the factor.
    fn cascade(self, scale: impl Fn(f64) -> f64) -> Result<Self, IntervalError> {
        let month_product = scale(f64::from(self.months));
        if !fits_in_i32(month_product) {
            return Err(IntervalError::OutOfRange);
        }
        let months = truncate_to_i32(month_product);
        let day_product = scale(f64::from(self.days));
        if !fits_in_i32(day_product) {
            return Err(IntervalError::OutOfRange);
        }
        let mut days = truncate_to_i32(day_product);
        // Under 30 in magnitude, the fraction of a month as days.
        let month_remainder_days = ts_round((month_product - f64::from(months)) * DAYS_PER_MONTH);
        let whole_remainder_days = truncate_to_i32(month_remainder_days);
        let mut sec_remainder = ts_round(
            (day_product - f64::from(days) + month_remainder_days
                - f64::from(whole_remainder_days))
                * SECS_PER_DAY,
        );
        if sec_remainder.abs() >= SECS_PER_DAY {
            // Under two days in magnitude, from two fractions below one day each.
            let carried = truncate_to_i32(sec_remainder / SECS_PER_DAY);
            days = days.checked_add(carried).ok_or(IntervalError::OutOfRange)?;
            sec_remainder -= f64::from(carried) * SECS_PER_DAY;
        }
        days = days
            .checked_add(whole_remainder_days)
            .ok_or(IntervalError::OutOfRange)?;
        let microseconds = round_ties_even(
            scale(int_to_f64(self.microseconds)) + sec_remainder * int_to_f64(USECS_PER_SEC),
        );
        if !fits_in_i64(microseconds) {
            return Err(IntervalError::OutOfRange);
        }
        Self {
            months,
            days,
            microseconds: truncate_to_i64(microseconds),
        }
        .finite()
    }

    /// PostgreSQL's `timestamp_pl_interval`, `at + self`: months added to the calendar month with
    /// the day clamped to the month's last, then days added to the calendar day, then the clock.
    ///
    /// The timestamp is read to the microsecond, as PostgreSQL holds it.
    ///
    /// # Errors
    ///
    /// [`ShiftError::TimestampOutOfRange`] where `at` or a step lies outside PostgreSQL's
    /// timestamps, and [`ShiftError::Unrepresentable`] where PostgreSQL answers `infinity`,
    /// `-infinity` or a year past `chrono`'s.
    ///
    /// ```
    /// use sql_values::{parse_interval, parse_timestamp};
    ///
    /// let at = parse_timestamp("2024-01-31 10:00:00").unwrap();
    /// let month = parse_interval("1 mon").unwrap();
    /// assert_eq!(month.add_to_timestamp(at).unwrap().to_string(), "2024-02-29 10:00:00");
    /// ```
    pub fn add_to_timestamp(self, at: NaiveDateTime) -> Result<NaiveDateTime, ShiftError> {
        let timestamp = pg_timestamp(at)?;
        if !self.is_finite() {
            return Err(ShiftError::Unrepresentable);
        }
        let mut timestamp = timestamp;
        if self.months != 0 {
            let (mut year, month, mut day, clock) = timestamp2tm(timestamp);
            let mut month = month
                .checked_add(self.months)
                .ok_or(ShiftError::TimestampOutOfRange)?;
            // C's truncating division and remainder.
            if month > MONTHS_PER_YEAR {
                year += (month - 1) / MONTHS_PER_YEAR;
                month = (month - 1) % MONTHS_PER_YEAR + 1;
            } else if month < 1 {
                year += month / MONTHS_PER_YEAR - 1;
                month = month % MONTHS_PER_YEAR + MONTHS_PER_YEAR;
            }
            day = day.min(days_in_month(year, month));
            timestamp = tm2timestamp(year, month, day, clock)?;
        }
        if self.days != 0 {
            let (year, month, day, clock) = timestamp2tm(timestamp);
            let julian = date2j(year, month, day)
                .checked_add(self.days)
                .filter(|julian| *julian >= 0)
                .ok_or(ShiftError::TimestampOutOfRange)?;
            let (year, month, day) = j2date(julian);
            timestamp = tm2timestamp(year, month, day, clock)?;
        }
        let timestamp = timestamp
            .checked_add(self.microseconds)
            .filter(|timestamp| (MIN_TIMESTAMP..END_TIMESTAMP).contains(timestamp))
            .ok_or(ShiftError::TimestampOutOfRange)?;
        chrono_timestamp(timestamp)
    }

    /// PostgreSQL's `timestamp_mi_interval`, `at - self`, which adds the negated interval.
    ///
    /// # Errors
    ///
    /// [`ShiftError::IntervalOutOfRange`] where `interval_um` refuses the interval, and the
    /// errors of [`add_to_timestamp`](Self::add_to_timestamp).
    pub fn subtract_from_timestamp(self, at: NaiveDateTime) -> Result<NaiveDateTime, ShiftError> {
        self.checked_neg()
            .map_err(|_| ShiftError::IntervalOutOfRange)?
            .add_to_timestamp(at)
    }

    /// PostgreSQL's `timestamp_mi`, `later - earlier`, with whole days carried into the days
    /// field as `interval_justify_hours` carries them.
    ///
    /// # Errors
    ///
    /// [`ShiftError::TimestampOutOfRange`] where a timestamp lies outside PostgreSQL's.
    ///
    /// ```
    /// use sql_values::{PgInterval, parse_timestamp};
    ///
    /// let later = parse_timestamp("2024-03-01 06:00:00").unwrap();
    /// let earlier = parse_timestamp("2024-02-28 12:00:00").unwrap();
    /// let span = PgInterval::timestamp_difference(later, earlier).unwrap();
    /// assert_eq!(span.to_string(), "1 day 18:00:00");
    /// ```
    pub fn timestamp_difference(
        later: NaiveDateTime,
        earlier: NaiveDateTime,
    ) -> Result<Self, ShiftError> {
        // Both lie within PostgreSQL's range, so their difference fits.
        let time = pg_timestamp(later)? - pg_timestamp(earlier)?;
        // Division truncates toward zero, so the days and the clock share the sign of the
        // difference and the sign corrections of `interval_justify_hours` never apply.
        // Valid timestamps lie under 2^27 days apart, which `int` holds.
        let days =
            i32::try_from(time / USECS_PER_DAY).expect("timestamps lie under 2^27 days apart");
        Ok(Self {
            months: 0,
            days,
            microseconds: time % USECS_PER_DAY,
        })
    }

    /// PostgreSQL's `time_pl_interval`, `at + self`: the clock moved by the interval's
    /// microseconds, wrapping in 64 bits as PostgreSQL does, then within the day. The months and
    /// days are ignored.
    ///
    /// # Errors
    ///
    /// [`ShiftError::InfiniteIntervalAddedToTime`] for an infinite interval.
    ///
    /// ```
    /// use sql_values::{parse_interval, parse_time};
    ///
    /// let at = parse_time("23:00:00").unwrap();
    /// let span = parse_interval("1 day 02:30:00").unwrap();
    /// assert_eq!(span.add_to_time(at).unwrap().to_string(), "01:30:00");
    /// ```
    pub fn add_to_time(self, at: NaiveTime) -> Result<NaiveTime, ShiftError> {
        if !self.is_finite() {
            return Err(ShiftError::InfiniteIntervalAddedToTime);
        }
        Ok(wrap_time(pg_time(at).wrapping_add(self.microseconds)))
    }

    /// PostgreSQL's `time_mi_interval`, `at - self`, as [`add_to_time`](Self::add_to_time) with
    /// the clock subtracted.
    ///
    /// # Errors
    ///
    /// [`ShiftError::InfiniteIntervalSubtractedFromTime`] for an infinite interval.
    pub fn subtract_from_time(self, at: NaiveTime) -> Result<NaiveTime, ShiftError> {
        if !self.is_finite() {
            return Err(ShiftError::InfiniteIntervalSubtractedFromTime);
        }
        Ok(wrap_time(pg_time(at).wrapping_sub(self.microseconds)))
    }
}

/// The three fields of an interval result, each `None` where its arithmetic overflowed.
struct FieldResults {
    months: Option<i32>,
    days: Option<i32>,
    microseconds: Option<i64>,
}

/// A time as PostgreSQL holds it, microseconds since midnight.
fn pg_time(at: NaiveTime) -> i64 {
    i64::from(at.num_seconds_from_midnight()) * USECS_PER_SEC + i64::from(at.nanosecond() / 1_000)
}

/// The time of `moved` microseconds reduced into the day, as `time_pl_interval` reduces it.
fn wrap_time(moved: i64) -> NaiveTime {
    let micros = moved.rem_euclid(USECS_PER_DAY);
    // `rem_euclid` keeps `micros` within one day, which both parts of the clock fit.
    let seconds = u32::try_from(micros / USECS_PER_SEC).expect("a second of the day fits u32");
    let nanos = u32::try_from(micros % USECS_PER_SEC * 1_000).expect("a nanosecond fits u32");
    NaiveTime::from_num_seconds_from_midnight_opt(seconds, nanos)
        .expect("a clock within the day is a time")
}

/// A timestamp as PostgreSQL holds it, microseconds from 2000-01-01.
pub(crate) fn pg_timestamp(at: NaiveDateTime) -> Result<i64, ShiftError> {
    valid_timestamp(pg_microseconds(at))
}

/// Microseconds from 2000-01-01 to `at`, which may lie outside PostgreSQL's range.
pub(crate) fn pg_microseconds(at: NaiveDateTime) -> i64 {
    let days = i64::from(at.date().num_days_from_ce()) + JULIAN_BEFORE_CE - POSTGRES_EPOCH_JDATE;
    // `chrono` holds under 2^27 days either side, so the microseconds fit.
    days * USECS_PER_DAY + pg_time(at.time())
}

/// `IS_VALID_TIMESTAMP`.
pub(crate) fn valid_timestamp(timestamp: i64) -> Result<i64, ShiftError> {
    if (MIN_TIMESTAMP..END_TIMESTAMP).contains(&timestamp) {
        Ok(timestamp)
    } else {
        Err(ShiftError::TimestampOutOfRange)
    }
}

/// The `chrono` timestamp of one PostgreSQL holds.
pub(crate) fn chrono_timestamp(timestamp: i64) -> Result<NaiveDateTime, ShiftError> {
    let days = timestamp.div_euclid(USECS_PER_DAY) + POSTGRES_EPOCH_JDATE - JULIAN_BEFORE_CE;
    let date = i32::try_from(days)
        .ok()
        .and_then(NaiveDate::from_num_days_from_ce_opt)
        .ok_or(ShiftError::Unrepresentable)?;
    Ok(date.and_time(wrap_time(timestamp)))
}

/// `timestamp2tm`, the year, month, day and clock of a valid timestamp.
fn timestamp2tm(timestamp: i64) -> (i32, i32, i32, i64) {
    let julian = timestamp.div_euclid(USECS_PER_DAY) + POSTGRES_EPOCH_JDATE;
    // A valid timestamp lies between Julian days 0 and 109203528.
    let julian = i32::try_from(julian).expect("a valid timestamp's Julian day fits int");
    let (year, month, day) = j2date(julian);
    (year, month, day, timestamp.rem_euclid(USECS_PER_DAY))
}

/// `tm2timestamp`, refusing a date before Julian day 0 or a timestamp out of range.
fn tm2timestamp(year: i32, month: i32, day: i32, clock: i64) -> Result<i64, ShiftError> {
    // `IS_VALID_JULIAN`, which keeps `date2j` from overflowing.
    let valid_julian = (year > -4713 || (year == -4713 && month >= 11))
        && (year < 5_874_898 || (year == 5_874_898 && month < 6));
    if !valid_julian {
        return Err(ShiftError::TimestampOutOfRange);
    }
    let days = i64::from(date2j(year, month, day)) - POSTGRES_EPOCH_JDATE;
    days.checked_mul(USECS_PER_DAY)
        .and_then(|micros| micros.checked_add(clock))
        .filter(|timestamp| (MIN_TIMESTAMP..END_TIMESTAMP).contains(timestamp))
        .ok_or(ShiftError::TimestampOutOfRange)
}

/// `date2j`, the Julian day of a proleptic Gregorian date within `IS_VALID_JULIAN`.
const fn date2j(year: i32, month: i32, day: i32) -> i32 {
    let (year, month) = if month > 2 {
        (year + 4800, month + 1)
    } else {
        (year + 4799, month + 13)
    };
    let century = year / 100;
    let mut julian = year * 365 - 32_167;
    julian += year / 4 - century + century / 4;
    julian + 7_834 * month / 256 + day
}

/// `j2date`, the proleptic Gregorian date of a Julian day from 0.
const fn j2date(julian_day: i32) -> (i32, i32, i32) {
    // Unsigned as in C, where every intermediate stays below 2^32.
    let mut julian = julian_day.cast_unsigned() + 32_044;
    let mut quad = julian / 146_097;
    let extra = (julian - quad * 146_097) * 4 + 3;
    julian += 60 + quad * 3 + extra / 146_097;
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
const fn days_in_month(year: i32, month: i32) -> i32 {
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// The running state of PostgreSQL's `sum(interval)` and `avg(interval)`.
///
/// The finite values are summed exactly per field and the infinities are counted, as PostgreSQL
/// counts them. PostgreSQL adds in scan order and raises on a partial sum leaving a field's range,
/// so the same rows can sum or raise by the order they are read in. Whenever a field's net total
/// fits, some order of the rows never leaves the range, and its answer is that net total, which is
/// the one reported here.
///
/// The counts are signed so that a row leaving is the same type as one arriving, and folding
/// either in is componentwise addition.
///
/// ```
/// use sql_values::{IntervalSum, PgInterval, parse_interval};
///
/// let mut total = IntervalSum::default();
/// for text in ["1 mon", "2 days", "-01:00:00"] {
///     total.merge(IntervalSum::of(parse_interval(text).unwrap(), 1));
/// }
/// assert_eq!(total.sum().unwrap().unwrap().to_string(), "1 mon 2 days -01:00:00");
/// assert_eq!(total.avg().unwrap().unwrap().to_string(), "10 days 15:40:00");
///
/// total.merge(IntervalSum::of(PgInterval::INFINITY, 1));
/// assert_eq!(total.avg(), Ok(Some(PgInterval::INFINITY)));
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct IntervalSum {
    finite: i64,
    positive_infinities: i64,
    negative_infinities: i64,
    months: i128,
    days: i128,
    microseconds: i128,
}

impl IntervalSum {
    /// One row's interval contributing with `weight`, `1` for a row arriving and `-1` for one
    /// leaving.
    #[must_use]
    pub fn of(value: PgInterval, weight: i64) -> Self {
        match value.infinity() {
            Some(Ordering::Greater) => Self {
                positive_infinities: weight,
                ..Self::default()
            },
            Some(_) => Self {
                negative_infinities: weight,
                ..Self::default()
            },
            None => {
                let wide = i128::from(weight);
                Self {
                    finite: weight,
                    positive_infinities: 0,
                    negative_infinities: 0,
                    months: i128::from(value.months) * wide,
                    days: i128::from(value.days) * wide,
                    microseconds: i128::from(value.microseconds) * wide,
                }
            }
        }
    }

    /// The state a database reports for some rows: the sum of the finite ones, how many there
    /// are, and how many are each infinity.
    #[must_use]
    pub fn seeded(
        finite_sum: Option<PgInterval>,
        finite: i64,
        positive_infinities: i64,
        negative_infinities: i64,
    ) -> Self {
        let (months, days, microseconds) =
            finite_sum.map_or((0, 0, 0), |sum| (sum.months, sum.days, sum.microseconds));
        Self {
            finite,
            positive_infinities,
            negative_infinities,
            months: i128::from(months),
            days: i128::from(days),
            microseconds: i128::from(microseconds),
        }
    }

    /// Fold `other` in, componentwise.
    pub const fn merge(&mut self, other: Self) {
        self.finite += other.finite;
        self.positive_infinities += other.positive_infinities;
        self.negative_infinities += other.negative_infinities;
        self.months += other.months;
        self.days += other.days;
        self.microseconds += other.microseconds;
    }

    /// Whether this moves nothing.
    #[must_use]
    pub fn is_zero(&self) -> bool {
        *self == Self::default()
    }

    /// The rows folded in.
    #[must_use]
    pub const fn count(&self) -> i64 {
        self.finite + self.positive_infinities + self.negative_infinities
    }

    /// PostgreSQL's `interval_sum`, `None` over no rows.
    ///
    /// # Errors
    ///
    /// [`IntervalError::OutOfRange`] for infinities of both signs and for a field whose net total
    /// leaves its range.
    pub fn sum(&self) -> Result<Option<PgInterval>, IntervalError> {
        if self.count() <= 0 {
            return Ok(None);
        }
        match (self.positive_infinities > 0, self.negative_infinities > 0) {
            (true, true) => Err(IntervalError::OutOfRange),
            (true, false) => Ok(Some(PgInterval::INFINITY)),
            (false, true) => Ok(Some(PgInterval::NEG_INFINITY)),
            (false, false) => PgInterval::finite_fields(FieldResults {
                months: i32::try_from(self.months).ok(),
                days: i32::try_from(self.days).ok(),
                microseconds: i64::try_from(self.microseconds).ok(),
            })
            .map(Some),
        }
    }

    /// PostgreSQL's `interval_avg`, the sum divided by the count of finite rows as
    /// `interval_div` divides, `None` over no rows.
    ///
    /// # Errors
    ///
    /// The errors of [`sum`](Self::sum), and [`IntervalError::OutOfRange`] for a quotient beyond
    /// the type.
    pub fn avg(&self) -> Result<Option<PgInterval>, IntervalError> {
        match self.sum()? {
            Some(sum) if sum.is_finite() => {
                // Intended loss, as PostgreSQL divides by the count as a `double`.
                let count = self.finite as f64;
                sum.checked_div(count).map(Some)
            }
            other => Ok(other),
        }
    }
}
