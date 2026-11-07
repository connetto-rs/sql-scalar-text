//! PostgreSQL 18's integer types, `smallint`, `integer` and `bigint`.

use core::ops::RangeInclusive;

use crate::cast::{CastError, CastTarget, CastValue, PgType};

/// A PostgreSQL integer type, ordered from the narrowest to the widest.
///
/// ```
/// use sql_values::{CastError, CastTarget, CastValue, IntegerType, PgType, cast_value};
///
/// assert_eq!(IntegerType::Smallint.range(), -32_768..=32_767);
/// assert_eq!(IntegerType::Smallint.wider(IntegerType::Integer), IntegerType::Integer);
/// assert_eq!(IntegerType::Integer.value(7), Ok(CastValue::Integer(7)));
/// assert_eq!(
///     IntegerType::Smallint.value(40_000),
///     Err(CastError::OutOfRange(PgType::Smallint)),
/// );
/// assert_eq!(IntegerType::try_from(PgType::Bigint), Ok(IntegerType::Bigint));
/// assert_eq!(IntegerType::try_from(CastTarget::Text), Err(CastTarget::Text));
///
/// // `value` is the cast from `bigint` to the type.
/// for integer in [IntegerType::Smallint, IntegerType::Integer, IntegerType::Bigint] {
///     assert_eq!(cast_value(CastValue::Bigint(7), integer.into()), integer.value(7));
/// }
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum IntegerType {
    /// `smallint`, 16 bits.
    Smallint,
    /// `integer`, 32 bits.
    Integer,
    /// `bigint`, 64 bits.
    Bigint,
}

impl IntegerType {
    /// The values the type holds.
    #[must_use]
    pub fn range(self) -> RangeInclusive<i64> {
        match self {
            Self::Smallint => i64::from(i16::MIN)..=i64::from(i16::MAX),
            Self::Integer => i64::from(i32::MIN)..=i64::from(i32::MAX),
            Self::Bigint => i64::MIN..=i64::MAX,
        }
    }

    /// The type of an arithmetic operator's result over the two types, the wider of them, as
    /// `int24pl` returns an `integer` and `int48mul` a `bigint`.
    #[must_use]
    pub fn wider(self, other: Self) -> Self {
        self.max(other)
    }

    /// The value held as the type, or the error a cast to the type raises for a value past its
    /// range.
    ///
    /// # Errors
    ///
    /// [`CastError::OutOfRange`] for a value outside [`IntegerType::range`].
    pub fn value(self, value: i64) -> Result<CastValue, CastError> {
        let out_of_range = |_| CastError::OutOfRange(self.into());
        match self {
            Self::Smallint => i16::try_from(value)
                .map(CastValue::Smallint)
                .map_err(out_of_range),
            Self::Integer => i32::try_from(value)
                .map(CastValue::Integer)
                .map_err(out_of_range),
            Self::Bigint => Ok(CastValue::Bigint(value)),
        }
    }
}

impl From<IntegerType> for PgType {
    fn from(integer: IntegerType) -> Self {
        match integer {
            IntegerType::Smallint => Self::Smallint,
            IntegerType::Integer => Self::Integer,
            IntegerType::Bigint => Self::Bigint,
        }
    }
}

impl From<IntegerType> for CastTarget {
    fn from(integer: IntegerType) -> Self {
        match integer {
            IntegerType::Smallint => Self::Smallint,
            IntegerType::Integer => Self::Integer,
            IntegerType::Bigint => Self::Bigint,
        }
    }
}

/// Refuses a type that is not an integer type, returning it.
impl TryFrom<PgType> for IntegerType {
    type Error = PgType;

    fn try_from(pg_type: PgType) -> Result<Self, Self::Error> {
        match pg_type {
            PgType::Smallint => Ok(Self::Smallint),
            PgType::Integer => Ok(Self::Integer),
            PgType::Bigint => Ok(Self::Bigint),
            other => Err(other),
        }
    }
}

/// Refuses a target that is not an integer type, returning it.
impl TryFrom<CastTarget> for IntegerType {
    type Error = CastTarget;

    fn try_from(target: CastTarget) -> Result<Self, Self::Error> {
        Self::try_from(target.pg_type()).map_err(|_| target)
    }
}
