//! PostgreSQL 18's `date` arithmetic, `date_pli`, `date_mii` and `date_mi`, the cast
//! `date2timestamp` and the cross-type comparison `date_cmp_timestamp`.

use core::cmp::Ordering;

use super::{DateTimeError, PgDate, PgTimestamp, TIMESTAMP_END_DAYS, USECS_PER_DAY, is_valid_date};

impl PgDate {
    /// `date + integer` and `integer + date`, PostgreSQL's `date_pli`, leaving an infinity as it
    /// is.
    ///
    /// # Errors
    ///
    /// [`DateTimeError::DateOutOfRange`] for a result before 4714-11-24 BC or from
    /// 5874898-01-01.
    ///
    /// ```
    /// use sql_values::{DateTimeError, PgDate};
    ///
    /// let date = PgDate::from_ymd(2026, 1, 31).unwrap();
    /// assert_eq!(date.checked_add_days(1).unwrap().to_string(), "2026-02-01");
    /// assert_eq!(PgDate::INFINITY.checked_add_days(1), Ok(PgDate::INFINITY));
    /// assert_eq!(date.checked_add_days(i32::MAX), Err(DateTimeError::DateOutOfRange));
    /// ```
    pub fn checked_add_days(self, days: i32) -> Result<Self, DateTimeError> {
        self.shift(self.0.checked_add(days))
    }

    /// `date - integer`, PostgreSQL's `date_mii`, leaving an infinity as it is.
    ///
    /// # Errors
    ///
    /// [`DateTimeError::DateOutOfRange`] for a result before 4714-11-24 BC or from
    /// 5874898-01-01.
    ///
    /// ```
    /// use sql_values::{DateTimeError, PgDate};
    ///
    /// let first = PgDate::from_ymd(-4713, 11, 24).unwrap();
    /// assert_eq!(first.checked_sub_days(1), Err(DateTimeError::DateOutOfRange));
    /// ```
    pub fn checked_sub_days(self, days: i32) -> Result<Self, DateTimeError> {
        self.shift(self.0.checked_sub(days))
    }

    /// `date - date`, PostgreSQL's `date_mi`, the days from `earlier` to `self`.
    ///
    /// # Errors
    ///
    /// [`DateTimeError::CannotSubtractInfiniteDates`] when either date is infinite.
    ///
    /// ```
    /// use sql_values::PgDate;
    ///
    /// let march = PgDate::from_ymd(2026, 3, 1).unwrap();
    /// let january = PgDate::from_ymd(2026, 1, 1).unwrap();
    /// assert_eq!(march.days_since(january), Ok(59));
    /// assert_eq!(january.days_since(march), Ok(-59));
    /// ```
    pub const fn days_since(self, earlier: Self) -> Result<i32, DateTimeError> {
        if self.is_finite() && earlier.is_finite() {
            // Finite dates lie under 2^31 days apart.
            Ok(self.0 - earlier.0)
        } else {
            Err(DateTimeError::CannotSubtractInfiniteDates)
        }
    }

    /// The `timestamp` at the start of the date, PostgreSQL's `date2timestamp`, with an infinite
    /// date the like infinite timestamp.
    ///
    /// # Errors
    ///
    /// [`DateTimeError::DateOutOfRangeForTimestamp`] for a date from 294277-01-01.
    ///
    /// ```
    /// use sql_values::{DateTimeError, PgDate, PgTimestamp};
    ///
    /// let date = PgDate::from_ymd(2026, 1, 1).unwrap();
    /// assert_eq!(date.to_timestamp().unwrap().to_string(), "2026-01-01 00:00:00");
    /// assert_eq!(PgDate::INFINITY.to_timestamp(), Ok(PgTimestamp::INFINITY));
    /// let late = PgDate::from_ymd(294_277, 1, 1).unwrap();
    /// assert_eq!(late.to_timestamp(), Err(DateTimeError::DateOutOfRangeForTimestamp));
    /// ```
    pub fn to_timestamp(self) -> Result<PgTimestamp, DateTimeError> {
        match self.0 {
            i32::MAX => Ok(PgTimestamp::INFINITY),
            i32::MIN => Ok(PgTimestamp::NEG_INFINITY),
            days if days >= TIMESTAMP_END_DAYS => Err(DateTimeError::DateOutOfRangeForTimestamp),
            days => Ok(PgTimestamp(i64::from(days) * USECS_PER_DAY)),
        }
    }

    /// The order of the date against a `timestamp`, PostgreSQL's `date_cmp_timestamp`, which
    /// never fails. A date past the last timestamp sorts after every finite timestamp and before
    /// `infinity`.
    ///
    /// ```
    /// use core::cmp::Ordering;
    /// use sql_values::{InputContext, PgDate, PgTimestamp};
    ///
    /// let date = PgDate::from_ymd(2026, 1, 1).unwrap();
    /// let morning = PgTimestamp::parse("2026-01-01 10:00", InputContext::new()).unwrap();
    /// assert_eq!(date.cmp_timestamp(morning), Ordering::Less);
    /// let last = PgDate::from_ymd(5_874_897, 12, 31).unwrap();
    /// assert_eq!(last.cmp_timestamp(morning), Ordering::Greater);
    /// assert_eq!(last.cmp_timestamp(PgTimestamp::INFINITY), Ordering::Less);
    /// ```
    #[must_use]
    pub fn cmp_timestamp(self, at: PgTimestamp) -> Ordering {
        match self.to_timestamp() {
            Ok(start) => start.cmp(&at),
            Err(_) if at == PgTimestamp::INFINITY => Ordering::Less,
            Err(_) => Ordering::Greater,
        }
    }

    /// `date_pli`'s result, an infinity left as it is and an overflow or a date outside
    /// PostgreSQL's refused.
    fn shift(self, shifted: Option<i32>) -> Result<Self, DateTimeError> {
        if !self.is_finite() {
            return Ok(self);
        }
        match shifted {
            Some(days) if is_valid_date(days) => Ok(Self(days)),
            _ => Err(DateTimeError::DateOutOfRange),
        }
    }
}
