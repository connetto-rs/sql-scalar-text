//! A known PostgreSQL value of one of the types whose comparisons need no setting, compared as
//! PostgreSQL 18's comparison operators compare it, across types where it has an operator between
//! them.

use alloc::string::String;
use core::{
    cmp::Ordering,
    fmt,
    hash::{Hash, Hasher},
};

use crate::{
    cast::{CastError, CastValue},
    datetime::{PgDate, PgTimestamp},
    integer::IntegerType,
    numeric::PgNumeric,
};

/// A known `boolean`, integer, `numeric`, `text`, `date` or `timestamp` value, equal, ordered and
/// hashed as PostgreSQL's comparison operators compare it.
///
/// An integer of every width and a `numeric` compare by value, as `int48eq`, or `numeric_eq` after
/// `int8_numeric`, compares them, so `4`, `4::bigint` and `4.00` are one value. A `date` compares
/// with a `timestamp` as `date_cmp_timestamp` does, equal to its midnight. A `text` compares by
/// its bytes, as under the `C` collation. PostgreSQL has no operator between a `boolean`, a
/// number, a `text` and a `date` or `timestamp`, so those are never equal and order in that
/// sequence.
///
/// Its [`Display`](fmt::Display) prints the type's output function under the default `DateStyle`
/// of `ISO`, as [`CastValue`]'s does.
///
/// ```
/// extern crate alloc;
///
/// use alloc::collections::BTreeSet;
///
/// use sql_values::{InputContext, PgDate, PgNumeric, PgTimestamp, PgValue};
///
/// let four = PgValue::Numeric(PgNumeric::parse("4.00").unwrap());
/// assert_eq!(PgValue::Integer(4), four);
/// assert!(PgValue::Integer(5) > four);
/// assert_eq!(BTreeSet::from([PgValue::Integer(4), four]).len(), 1);
///
/// let day = PgValue::Date(PgDate::from_ymd(2026, 1, 1).unwrap());
/// let midnight = PgTimestamp::parse("2026-01-01 00:00", InputContext::new()).unwrap();
/// assert_eq!(day, PgValue::Timestamp(midnight));
///
/// assert!(PgValue::Text("B".into()) < PgValue::Text("a".into()));
/// assert!(PgValue::Boolean(true) < PgValue::Integer(0));
/// assert_eq!(PgValue::Boolean(true).to_string(), "t");
/// ```
#[derive(Debug, Clone)]
pub enum PgValue {
    /// A `boolean`, `false` before `true`.
    Boolean(bool),
    /// A `smallint`, `integer` or `bigint`, whichever its width.
    Integer(i64),
    /// A `numeric`.
    Numeric(PgNumeric),
    /// A `text` or `varchar`.
    Text(String),
    /// A `date`.
    Date(PgDate),
    /// A `timestamp`.
    Timestamp(PgTimestamp),
}

impl PgValue {
    /// The rank of the value's types among the ones PostgreSQL has no operator between.
    const fn family(&self) -> u8 {
        match self {
            Self::Boolean(_) => 0,
            Self::Integer(_) | Self::Numeric(_) => 1,
            Self::Text(_) => 2,
            Self::Date(_) | Self::Timestamp(_) => 3,
        }
    }

    /// The value as a [`CastValue`], an integer held as `integer`.
    ///
    /// # Errors
    ///
    /// [`CastError::OutOfRange`] for an integer outside [`IntegerType::range`].
    ///
    /// ```
    /// use sql_values::{CastError, CastValue, IntegerType, PgType, PgValue};
    ///
    /// let value = || PgValue::Integer(40_000);
    /// let held = value().into_cast_value(IntegerType::Integer);
    /// assert_eq!(held, Ok(CastValue::Integer(40_000)));
    /// assert_eq!(
    ///     value().into_cast_value(IntegerType::Smallint),
    ///     Err(CastError::OutOfRange(PgType::Smallint)),
    /// );
    /// assert_eq!(PgValue::try_from(CastValue::Smallint(7)).unwrap(), PgValue::Integer(7));
    /// ```
    pub fn into_cast_value(self, integer: IntegerType) -> Result<CastValue, CastError> {
        Ok(match self {
            Self::Boolean(value) => CastValue::Boolean(value),
            Self::Integer(value) => return integer.value(value),
            Self::Numeric(value) => CastValue::Numeric(value),
            Self::Text(text) => CastValue::Text(text),
            Self::Date(date) => CastValue::Date(date),
            Self::Timestamp(at) => CastValue::Timestamp(at),
        })
    }

    /// The `numeric` of an integer, as `int8_numeric` makes it, or a `numeric` itself.
    #[must_use]
    pub fn as_numeric(&self) -> Option<PgNumeric> {
        match self {
            Self::Integer(value) => Some(PgNumeric::from(*value)),
            Self::Numeric(value) => Some(value.clone()),
            _ => None,
        }
    }

    /// The text of a `text`.
    #[must_use]
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            _ => None,
        }
    }

    /// A `date`.
    #[must_use]
    pub const fn as_date(&self) -> Option<PgDate> {
        match self {
            Self::Date(date) => Some(*date),
            _ => None,
        }
    }

    /// The `timestamp` the value equals, a `timestamp` itself or a `date`'s midnight, which a date
    /// from 294277-01-01 has none of.
    ///
    /// ```
    /// use sql_values::{PgDate, PgValue};
    ///
    /// let day = PgValue::Date(PgDate::from_ymd(2026, 1, 1).unwrap());
    /// assert_eq!(day.as_timestamp().unwrap().to_string(), "2026-01-01 00:00:00");
    /// let late = PgValue::Date(PgDate::from_ymd(294_277, 1, 1).unwrap());
    /// assert_eq!(late.as_timestamp(), None);
    /// ```
    #[must_use]
    pub fn as_timestamp(&self) -> Option<PgTimestamp> {
        match self {
            Self::Date(date) => date.to_timestamp().ok(),
            Self::Timestamp(at) => Some(*at),
            _ => None,
        }
    }

    /// The type's output function, as `concat` and `||` call it, or `None` for a `date` or a
    /// `timestamp`, whose spelling `DateStyle` decides.
    ///
    /// ```
    /// use sql_values::{PgDate, PgValue};
    ///
    /// let flag = PgValue::Boolean(false);
    /// assert_eq!(flag.output().map(|output| output.to_string()).as_deref(), Some("f"));
    /// assert!(PgValue::Date(PgDate::from_ymd(2026, 1, 1).unwrap()).output().is_none());
    /// ```
    #[must_use]
    pub fn output(&self) -> Option<impl fmt::Display + '_> {
        (!matches!(self, Self::Date(_) | Self::Timestamp(_))).then_some(self)
    }
}

/// Refuses a `char(n)` or a `timestamptz`, returning it.
impl TryFrom<CastValue> for PgValue {
    type Error = CastValue;

    fn try_from(value: CastValue) -> Result<Self, Self::Error> {
        Ok(match value {
            CastValue::Boolean(value) => Self::Boolean(value),
            CastValue::Smallint(value) => Self::Integer(value.into()),
            CastValue::Integer(value) => Self::Integer(value.into()),
            CastValue::Bigint(value) => Self::Integer(value),
            CastValue::Numeric(value) => Self::Numeric(value),
            CastValue::Text(text) => Self::Text(text),
            CastValue::Date(date) => Self::Date(date),
            CastValue::Timestamp(at) => Self::Timestamp(at),
            CastValue::Bpchar(_) | CastValue::TimestampTz(_) => return Err(value),
        })
    }
}

impl PartialEq for PgValue {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for PgValue {}

impl PartialOrd for PgValue {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PgValue {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Boolean(left), Self::Boolean(right)) => left.cmp(right),
            (Self::Integer(left), Self::Integer(right)) => left.cmp(right),
            (Self::Integer(left), Self::Numeric(right)) => right.cmp_integer(*left).reverse(),
            (Self::Numeric(left), Self::Integer(right)) => left.cmp_integer(*right),
            (Self::Numeric(left), Self::Numeric(right)) => left.cmp(right),
            (Self::Text(left), Self::Text(right)) => left.cmp(right),
            (Self::Date(left), Self::Date(right)) => left.cmp(right),
            (Self::Date(left), Self::Timestamp(right)) => left.cmp_timestamp(*right),
            (Self::Timestamp(left), Self::Date(right)) => right.cmp_timestamp(*left).reverse(),
            (Self::Timestamp(left), Self::Timestamp(right)) => left.cmp(right),
            (left, right) => left.family().cmp(&right.family()),
        }
    }
}

impl Hash for PgValue {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.family().hash(state);
        match self {
            Self::Boolean(value) => value.hash(state),
            Self::Integer(value) => PgNumeric::hash_integer(*value, state),
            Self::Numeric(value) => value.hash(state),
            Self::Text(text) => text.hash(state),
            // A date past every timestamp equals none of them.
            Self::Date(date) => match date.to_timestamp() {
                Ok(at) => at.hash(state),
                Err(_) => date.hash(state),
            },
            Self::Timestamp(at) => at.hash(state),
        }
    }
}

impl fmt::Display for PgValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Boolean(value) => f.write_str(if *value { "t" } else { "f" }),
            Self::Integer(value) => write!(f, "{value}"),
            Self::Numeric(value) => write!(f, "{value}"),
            Self::Text(text) => f.write_str(text),
            Self::Date(date) => write!(f, "{date}"),
            Self::Timestamp(at) => write!(f, "{at}"),
        }
    }
}
