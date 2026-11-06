//! PostgreSQL 18's arithmetic on intervals, ported from `timestamp.c` and `date.c` with
//! PostgreSQL's order of checks.

use core::cmp::Ordering;

use super::{IntervalError, PgInterval};
use crate::{
    DateTimeError, PgTime, PgTimeZone, PgTimestamp, PgTimestampTz,
    datetime::{
        MONTHS_PER_YEAR, POSTGRES_EPOCH_JDATE, USECS_PER_DAY, USECS_PER_SEC, date2j, days_in_month,
        is_valid_julian, is_valid_timestamp, j2date, local_offset, timestamp2tm, tm2timestamp,
        utc_offset,
    },
    float::{
        fits_in_i32, fits_in_i64, int_to_f64, round_ties_even, truncate_to_i32, truncate_to_i64,
        ts_round,
    },
};

const DAYS_PER_MONTH: f64 = 30.0;
const SECS_PER_DAY: f64 = 86_400.0;

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
    /// An IANA time zone asked about an instant outside jiff's years -9999 to 9999.
    #[error("value outside jiff's years -9999 to 9999")]
    OutsideJiffRange,
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
    /// An infinite timestamp or interval gives an infinity.
    ///
    /// # Errors
    ///
    /// [`ShiftError::TimestampOutOfRange`] where a step leaves PostgreSQL's timestamps, or where
    /// opposite infinities meet.
    ///
    /// ```
    /// use sql_values::{parse_interval, parse_timestamp};
    ///
    /// let at = parse_timestamp("2024-01-31 10:00:00").unwrap();
    /// let month = parse_interval("1 mon").unwrap();
    /// assert_eq!(month.add_to_timestamp(at).unwrap().to_string(), "2024-02-29 10:00:00");
    /// ```
    pub fn add_to_timestamp(self, at: PgTimestamp) -> Result<PgTimestamp, ShiftError> {
        let shifted = self.shift(at.pg_micros(), None)?;
        Ok(PgTimestamp::from_pg_micros(shifted).expect("a shifted timestamp is checked"))
    }

    /// PostgreSQL's `timestamp_mi_interval`, `at - self`, which adds the negated interval.
    ///
    /// # Errors
    ///
    /// [`ShiftError::IntervalOutOfRange`] where `interval_um` refuses the interval, and the
    /// errors of [`add_to_timestamp`](Self::add_to_timestamp).
    pub fn subtract_from_timestamp(self, at: PgTimestamp) -> Result<PgTimestamp, ShiftError> {
        self.checked_neg()
            .map_err(|_| ShiftError::IntervalOutOfRange)?
            .add_to_timestamp(at)
    }

    /// PostgreSQL's `timestamptz_pl_interval` with `zone` as the session's `TimeZone`,
    /// `at + self`, with months and days added on the zone's calendar, each step's local clock
    /// resolved as [`PgTimestamp::to_timestamptz`] resolves it, then the microseconds added to
    /// the instant.
    ///
    /// # Errors
    ///
    /// The errors of [`add_to_timestamp`](Self::add_to_timestamp), and
    /// [`ShiftError::OutsideJiffRange`] where an IANA zone is asked about an instant outside
    /// jiff's years.
    ///
    /// ```
    /// use sql_values::{InputContext, PgTimeZone, PgTimestampTz, parse_interval};
    ///
    /// let rome = PgTimeZone::parse("Europe/Rome").unwrap();
    /// let at = PgTimestampTz::parse("2026-03-29 12:00:00+02", InputContext::new()).unwrap();
    /// let day = parse_interval("1 day").unwrap();
    /// let before = day.subtract_from_timestamptz(at, &rome).unwrap();
    /// assert_eq!(before.to_string(), "2026-03-28 11:00:00+00");
    /// ```
    pub fn add_to_timestamptz(
        self,
        at: PgTimestampTz,
        zone: &PgTimeZone,
    ) -> Result<PgTimestampTz, ShiftError> {
        let shifted = self.shift(at.pg_micros(), Some(zone))?;
        Ok(PgTimestampTz::from_pg_micros(shifted).expect("a shifted timestamp is checked"))
    }

    /// PostgreSQL's `timestamptz_mi_interval`, `at - self`, which adds the negated interval.
    ///
    /// # Errors
    ///
    /// [`ShiftError::IntervalOutOfRange`] where `interval_um` refuses the interval, and the
    /// errors of [`add_to_timestamptz`](Self::add_to_timestamptz).
    pub fn subtract_from_timestamptz(
        self,
        at: PgTimestampTz,
        zone: &PgTimeZone,
    ) -> Result<PgTimestampTz, ShiftError> {
        self.checked_neg()
            .map_err(|_| ShiftError::IntervalOutOfRange)?
            .add_to_timestamptz(at, zone)
    }

    /// `timestamp_pl_interval`, or `timestamptz_pl_interval` in `zone`, over the stored
    /// microseconds.
    fn shift(self, at: i64, zone: Option<&PgTimeZone>) -> Result<i64, ShiftError> {
        let out_of_range = ShiftError::TimestampOutOfRange;
        match self.infinity() {
            Some(Ordering::Less) if at == i64::MAX => return Err(out_of_range),
            Some(Ordering::Less) => return Ok(i64::MIN),
            Some(Ordering::Greater) if at == i64::MIN => return Err(out_of_range),
            Some(_) => return Ok(i64::MAX),
            None if at == i64::MAX || at == i64::MIN => return Ok(at),
            None => {}
        }
        // `timestamp2tm`, in the zone when there is one.
        let local_fields = |utc: i64| -> Result<_, ShiftError> {
            let offset = match zone {
                None => 0,
                Some(zone) => utc_offset(zone, utc).map_err(shift_error)?,
            };
            Ok(timestamp2tm(utc + i64::from(offset) * USECS_PER_SEC))
        };
        // `DetermineTimeZoneOffset` in the zone when there is one, then `tm2timestamp`.
        let rebuild = |year: i32, month: i32, day: i32, clock: i64| {
            if !is_valid_julian(year, month) {
                return Err(out_of_range);
            }
            let offset = match zone {
                None => 0,
                Some(zone) => {
                    let days = i64::from(date2j(year, month, day) - POSTGRES_EPOCH_JDATE);
                    let local = days
                        .checked_mul(USECS_PER_DAY)
                        .and_then(|micros| micros.checked_add(clock))
                        .ok_or(out_of_range)?;
                    local_offset(zone, local).map_err(shift_error)?
                }
            };
            tm2timestamp(year, month, day, clock, offset).ok_or(out_of_range)
        };
        let mut timestamp = at;
        if self.months != 0 {
            let (mut year, month, mut day, clock) = local_fields(timestamp)?;
            let mut month = month.checked_add(self.months).ok_or(out_of_range)?;
            // C's truncating division and remainder.
            if month > MONTHS_PER_YEAR {
                year += (month - 1) / MONTHS_PER_YEAR;
                month = (month - 1) % MONTHS_PER_YEAR + 1;
            } else if month < 1 {
                year += month / MONTHS_PER_YEAR - 1;
                month = month % MONTHS_PER_YEAR + MONTHS_PER_YEAR;
            }
            day = day.min(days_in_month(year, month));
            timestamp = rebuild(year, month, day, clock)?;
        }
        if self.days != 0 {
            let (year, month, day, clock) = local_fields(timestamp)?;
            // `timestamp_pl_interval` refuses a Julian day below 0, and
            // `timestamptz_pl_interval` one below -1, which a local date west of UTC reaches.
            let lowest = if zone.is_some() { -1 } else { 0 };
            let julian = date2j(year, month, day)
                .checked_add(self.days)
                .filter(|julian| *julian >= lowest)
                .ok_or(out_of_range)?;
            let (year, month, day) = j2date(julian);
            timestamp = rebuild(year, month, day, clock)?;
        }
        timestamp
            .checked_add(self.microseconds)
            .filter(|&timestamp| is_valid_timestamp(timestamp))
            .ok_or(out_of_range)
    }

    /// PostgreSQL's `timestamp_mi`, `later - earlier`, with whole days carried into the days
    /// field as `interval_justify_hours` carries them. An infinite timestamp gives an infinite
    /// interval.
    ///
    /// # Errors
    ///
    /// [`ShiftError::IntervalOutOfRange`] where equal infinities meet, or where the difference
    /// passes 2^63 microseconds.
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
        later: PgTimestamp,
        earlier: PgTimestamp,
    ) -> Result<Self, ShiftError> {
        let (later, earlier) = (later.pg_micros(), earlier.pg_micros());
        let infinite = |micros: i64| micros == i64::MAX || micros == i64::MIN;
        if infinite(later) || infinite(earlier) {
            return match (later, earlier) {
                (i64::MIN, i64::MIN) | (i64::MAX, i64::MAX) => Err(ShiftError::IntervalOutOfRange),
                (i64::MIN, _) | (_, i64::MAX) => Ok(Self::NEG_INFINITY),
                _ => Ok(Self::INFINITY),
            };
        }
        // Timestamps at opposite ends of PostgreSQL's range lie more than 2^63 microseconds
        // apart.
        let time = later
            .checked_sub(earlier)
            .ok_or(ShiftError::IntervalOutOfRange)?;
        // Division truncates toward zero, so the days and the clock share the sign of the
        // difference and the sign corrections of `interval_justify_hours` never apply. 2^63
        // microseconds are under 2^27 days, which `int` holds.
        let days = i32::try_from(time / USECS_PER_DAY).expect("2^63 microseconds fit int days");
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
    pub fn add_to_time(self, at: PgTime) -> Result<PgTime, ShiftError> {
        if !self.is_finite() {
            return Err(ShiftError::InfiniteIntervalAddedToTime);
        }
        Ok(wrap_time(at.micros().wrapping_add(self.microseconds)))
    }

    /// PostgreSQL's `time_mi_interval`, `at - self`, as [`add_to_time`](Self::add_to_time) with
    /// the clock subtracted.
    ///
    /// # Errors
    ///
    /// [`ShiftError::InfiniteIntervalSubtractedFromTime`] for an infinite interval.
    pub fn subtract_from_time(self, at: PgTime) -> Result<PgTime, ShiftError> {
        if !self.is_finite() {
            return Err(ShiftError::InfiniteIntervalSubtractedFromTime);
        }
        Ok(wrap_time(at.micros().wrapping_sub(self.microseconds)))
    }
}

/// The three fields of an interval result, each `None` where its arithmetic overflowed.
struct FieldResults {
    months: Option<i32>,
    days: Option<i32>,
    microseconds: Option<i64>,
}

/// The shift error of a refusal by a zone lookup.
fn shift_error(error: DateTimeError) -> ShiftError {
    match error {
        DateTimeError::OutsideJiffRange => ShiftError::OutsideJiffRange,
        _ => ShiftError::TimestampOutOfRange,
    }
}

/// The time of `moved` microseconds reduced into the day, as `time_pl_interval` reduces it.
fn wrap_time(moved: i64) -> PgTime {
    PgTime::from_micros(moved.rem_euclid(USECS_PER_DAY)).expect("a clock within the day is a time")
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
