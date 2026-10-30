//! PostgreSQL `interval` values read by `interval_in`, printed by `interval_out` and compared by
//! `interval_cmp`.

mod decode;

use core::{
    cmp::Ordering,
    fmt,
    hash::{Hash, Hasher},
};

use decode::{
    INTERVAL_FULL_RANGE, RANGE_DAY, RANGE_HOUR, RANGE_MINUTE, RANGE_MONTH, RANGE_SECOND,
    RANGE_YEAR, interval_in,
};

const USECS_PER_DAY: i128 = 86_400_000_000;
const USECS_PER_SEC: u64 = 1_000_000;
const DAYS_PER_MONTH: i64 = 30;

/// A PostgreSQL `interval` as months, days and microseconds, equal, ordered and hashed by
/// PostgreSQL's span with a month as 30 days and a day as 24 hours.
///
/// ```
/// use sql_scalar_text::{PgInterval, parse_interval};
///
/// let month = parse_interval("1 mon").unwrap();
/// let thirty_days = parse_interval("30 days").unwrap();
/// assert_eq!(month, thirty_days);
/// assert_ne!(month.to_string(), thirty_days.to_string());
/// assert!(parse_interval("infinity").unwrap() > month);
/// assert_eq!(PgInterval::NEG_INFINITY.to_string(), "-infinity");
/// ```
#[derive(Debug, Clone, Copy)]
pub struct PgInterval {
    /// Months, twelve of which print as a year.
    pub months: i32,
    /// Days, never folded into months.
    pub days: i32,
    /// Microseconds, never folded into days.
    pub microseconds: i64,
}

impl PgInterval {
    /// `infinity`, PostgreSQL's `INTERVAL_NOEND`, above every finite interval.
    pub const INFINITY: Self = Self {
        months: i32::MAX,
        days: i32::MAX,
        microseconds: i64::MAX,
    };

    /// `-infinity`, PostgreSQL's `INTERVAL_NOBEGIN`, below every finite interval.
    pub const NEG_INFINITY: Self = Self {
        months: i32::MIN,
        days: i32::MIN,
        microseconds: i64::MIN,
    };

    /// Whether the interval is neither `infinity` nor `-infinity`.
    ///
    /// ```
    /// use sql_scalar_text::PgInterval;
    ///
    /// assert!(!PgInterval::INFINITY.is_finite());
    /// assert!(PgInterval { months: i32::MAX, days: i32::MAX, microseconds: 0 }.is_finite());
    /// ```
    #[must_use]
    pub const fn is_finite(&self) -> bool {
        !self.fields_equal(&Self::INFINITY) && !self.fields_equal(&Self::NEG_INFINITY)
    }

    const fn fields_equal(&self, other: &Self) -> bool {
        self.months == other.months
            && self.days == other.days
            && self.microseconds == other.microseconds
    }

    /// `interval_cmp_value`, the span in microseconds.
    fn span(&self) -> i128 {
        let days = i64::from(self.months) * DAYS_PER_MONTH + i64::from(self.days);
        i128::from(self.microseconds) + i128::from(days) * USECS_PER_DAY
    }
}

impl PartialEq for PgInterval {
    fn eq(&self, other: &Self) -> bool {
        self.span() == other.span()
    }
}

impl Eq for PgInterval {}

impl PartialOrd for PgInterval {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PgInterval {
    fn cmp(&self, other: &Self) -> Ordering {
        self.span().cmp(&other.span())
    }
}

impl Hash for PgInterval {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.span().hash(state);
    }
}

/// PostgreSQL's text under the default `postgres` `IntervalStyle`, which [`parse_interval`] reads
/// back except the clock of `i64::MIN` microseconds that PostgreSQL refuses too.
impl fmt::Display for PgInterval {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.fields_equal(&Self::INFINITY) {
            return f.write_str("infinity");
        }
        if self.fields_equal(&Self::NEG_INFINITY) {
            return f.write_str("-infinity");
        }
        // `INTSTYLE_POSTGRES` marks a positive count that follows a negative one with `+`.
        let mut is_zero = true;
        let mut is_before = false;
        for (count, unit) in [
            (self.months / 12, "year"),
            (self.months % 12, "mon"),
            (self.days, "day"),
        ] {
            if count == 0 {
                continue;
            }
            let separator = if is_zero { "" } else { " " };
            let plus = if is_before && count > 0 { "+" } else { "" };
            let plural = if count == 1 { "" } else { "s" };
            write!(f, "{separator}{plus}{count} {unit}{plural}")?;
            is_before = count < 0;
            is_zero = false;
        }
        if is_zero || self.microseconds != 0 {
            let separator = if is_zero { "" } else { " " };
            let sign = if self.microseconds < 0 {
                "-"
            } else if is_before {
                "+"
            } else {
                ""
            };
            let magnitude = self.microseconds.unsigned_abs();
            let seconds = magnitude / USECS_PER_SEC;
            write!(
                f,
                "{separator}{sign}{:02}:{:02}:{:02}",
                seconds / 3_600,
                seconds % 3_600 / 60,
                seconds % 60
            )?;
            let mut fraction = magnitude % USECS_PER_SEC;
            if fraction != 0 {
                let mut digits = 6;
                while fraction % 10 == 0 {
                    fraction /= 10;
                    digits -= 1;
                }
                write!(f, ".{fraction:0digits$}")?;
            }
        }
        Ok(())
    }
}

/// The fields an interval type keeps, as in `interval day to second`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IntervalFields {
    /// `YEAR`.
    Year,
    /// `MONTH`.
    Month,
    /// `DAY`.
    Day,
    /// `HOUR`.
    Hour,
    /// `MINUTE`.
    Minute,
    /// `SECOND`.
    Second,
    /// `YEAR TO MONTH`.
    YearToMonth,
    /// `DAY TO HOUR`.
    DayToHour,
    /// `DAY TO MINUTE`.
    DayToMinute,
    /// `DAY TO SECOND`.
    DayToSecond,
    /// `HOUR TO MINUTE`.
    HourToMinute,
    /// `HOUR TO SECOND`.
    HourToSecond,
    /// `MINUTE TO SECOND`.
    MinuteToSecond,
}

impl IntervalFields {
    /// `INTERVAL_MASK` of every field kept.
    const fn range(self) -> u16 {
        match self {
            Self::Year => RANGE_YEAR,
            Self::Month => RANGE_MONTH,
            Self::Day => RANGE_DAY,
            Self::Hour => RANGE_HOUR,
            Self::Minute => RANGE_MINUTE,
            Self::Second => RANGE_SECOND,
            Self::YearToMonth => RANGE_YEAR | RANGE_MONTH,
            Self::DayToHour => RANGE_DAY | RANGE_HOUR,
            Self::DayToMinute => RANGE_DAY | RANGE_HOUR | RANGE_MINUTE,
            Self::DayToSecond => RANGE_DAY | RANGE_HOUR | RANGE_MINUTE | RANGE_SECOND,
            Self::HourToMinute => RANGE_HOUR | RANGE_MINUTE,
            Self::HourToSecond => RANGE_HOUR | RANGE_MINUTE | RANGE_SECOND,
            Self::MinuteToSecond => RANGE_MINUTE | RANGE_SECOND,
        }
    }

    /// Whether a precision may follow, which only `SECOND` allows.
    const fn ends_in_second(self) -> bool {
        matches!(
            self,
            Self::Second | Self::DayToSecond | Self::HourToSecond | Self::MinuteToSecond
        )
    }
}

/// The modifier of an interval type, the fields it keeps and the fractional-second digits it
/// rounds to, where the default is plain `interval`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct IntervalTypmod {
    fields: Option<IntervalFields>,
    precision: Option<u8>,
}

/// The most fractional-second digits an interval keeps.
const MAX_INTERVAL_PRECISION: u8 = 6;

impl IntervalTypmod {
    /// `intervaltypmodin`, clamping a precision above six as PostgreSQL does, or `None` for a
    /// precision after fields not ending in `SECOND`.
    ///
    /// ```
    /// use sql_scalar_text::{IntervalFields, IntervalTypmod};
    ///
    /// assert!(IntervalTypmod::new(Some(IntervalFields::DayToSecond), Some(3)).is_some());
    /// assert!(IntervalTypmod::new(None, Some(3)).is_some());
    /// assert!(IntervalTypmod::new(Some(IntervalFields::Minute), Some(3)).is_none());
    /// ```
    #[must_use]
    pub const fn new(fields: Option<IntervalFields>, precision: Option<u8>) -> Option<Self> {
        let precision = match (fields, precision) {
            (Some(fields), Some(_)) if !fields.ends_in_second() => return None,
            (_, Some(precision)) if precision > MAX_INTERVAL_PRECISION => {
                Some(MAX_INTERVAL_PRECISION)
            }
            (_, precision) => precision,
        };
        Some(Self { fields, precision })
    }

    const fn range(self) -> u16 {
        match self.fields {
            Some(fields) => fields.range(),
            None => INTERVAL_FULL_RANGE,
        }
    }
}

/// Parse an interval as PostgreSQL 18's `interval_in` reads it under the default `IntervalStyle`,
/// or `None` where PostgreSQL raises an error.
///
/// ```
/// use sql_scalar_text::{PgInterval, parse_interval};
///
/// let interval = parse_interval("-1 days +02:00:00").unwrap();
/// assert_eq!(
///     (interval.months, interval.days, interval.microseconds),
///     (0, -1, 7_200_000_000),
/// );
/// assert_eq!(parse_interval("@ 1 hour ago").unwrap().to_string(), "-01:00:00");
/// assert_eq!(parse_interval("PT15M").unwrap().to_string(), "00:15:00");
/// assert_eq!(parse_interval("-infinity"), Some(PgInterval::NEG_INFINITY));
/// assert_eq!(parse_interval("1 fortnight"), None);
/// ```
#[must_use]
pub fn parse_interval(text: &str) -> Option<PgInterval> {
    interval_in(text, INTERVAL_FULL_RANGE, None)
}

/// Parse an interval literal of a restricted type as `interval '…' <fields>` reads it, which
/// differs from casting a text value to that type.
///
/// ```
/// use sql_scalar_text::{
///     IntervalFields, IntervalTypmod, parse_interval, parse_interval_typmod,
/// };
///
/// let minute = IntervalTypmod::new(Some(IntervalFields::Minute), None).unwrap();
/// assert_eq!(parse_interval_typmod("90", minute).unwrap().to_string(), "01:30:00");
///
/// let to_second = IntervalTypmod::new(Some(IntervalFields::MinuteToSecond), Some(0)).unwrap();
/// assert_eq!(parse_interval_typmod("1:30.5", to_second).unwrap().to_string(), "00:01:31");
///
/// let hour = IntervalTypmod::new(Some(IntervalFields::Hour), None).unwrap();
/// assert_eq!(parse_interval_typmod("3 4", hour).unwrap().to_string(), "3 days 04:00:00");
/// assert_eq!(parse_interval("3 4"), None);
/// ```
#[must_use]
pub fn parse_interval_typmod(text: &str, typmod: IntervalTypmod) -> Option<PgInterval> {
    interval_in(text, typmod.range(), typmod.precision)
}
