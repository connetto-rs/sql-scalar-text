//! PostgreSQL 18's explicit casts, of a literal, `'…'::type` or `1.5::type`, and of a typed
//! value, each the input function or the cast function `pg_cast` names followed by the length
//! coercion of the target's modifier, and the typing of a numeric constant.

use alloc::string::{String, ToString};
use core::fmt;

use crate::{
    bpchar::PgBpchar,
    datetime::{DateTimeError, InputContext, PgDate, PgTimestamp, PgTimestampTz},
    integer::IntegerType,
    numeric::{NumericError, NumericTypmod, PgNumeric},
};

/// A literal as written before `::`, its quotes and escapes already decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Literal<'a> {
    /// A string constant, `'…'`, `E'…'` or `$$…$$`, whose type PostgreSQL leaves unknown until
    /// the cast reads it with the target's input function.
    Quoted(&'a str),
    /// A numeric constant as PostgreSQL 18's lexer reads it, `42`, `0x2A`, `1_000`, `2.5`, `.5`
    /// or `1e2`, with a leading `-` for a negation the parser folds into the constant, as in
    /// `(-2.5)::int`.
    Number(&'a str),
}

/// The type a literal or value is cast to, with its modifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CastTarget {
    /// `boolean`.
    Boolean,
    /// `smallint`.
    Smallint,
    /// `integer`.
    Integer,
    /// `bigint`.
    Bigint,
    /// `numeric`, or `numeric(p, s)` with a modifier.
    Numeric(Option<NumericTypmod>),
    /// `text`.
    Text,
    /// `varchar`, or `varchar(n)` with a length.
    Varchar(Option<CharLength>),
    /// `bpchar`, or `char(n)` with a length, where a bare `char` is `char(1)`.
    Bpchar(Option<CharLength>),
    /// `date`.
    Date,
    /// `timestamp`, or `timestamp(p)` with a precision, where one above 6 is 6.
    Timestamp(Option<u8>),
    /// `timestamptz`, or `timestamptz(p)` with a precision, where one above 6 is 6.
    TimestampTz(Option<u8>),
}

/// The length of a `varchar(n)` or `char(n)` type, in characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CharLength(usize);

/// `MaxAttrSize`, the longest length a character type takes.
const MAX_CHAR_LENGTH: usize = 10_485_760;

impl CharLength {
    /// The length PostgreSQL accepts in `varchar(n)` and `char(n)`, from 1 to 10485760.
    ///
    /// ```
    /// use sql_values::CharLength;
    ///
    /// assert!(CharLength::new(1).is_some());
    /// assert!(CharLength::new(0).is_none());
    /// assert!(CharLength::new(10_485_761).is_none());
    /// ```
    #[must_use]
    pub fn new(length: usize) -> Option<Self> {
        (1..=MAX_CHAR_LENGTH)
            .contains(&length)
            .then_some(Self(length))
    }
}

/// A type PostgreSQL names in a cast's error, printed as `format_type` names it.
///
/// ```
/// use sql_values::{DateTimeError, PgType};
///
/// assert_eq!(PgType::Time.to_string(), "time without time zone");
/// // `time_in` names the type `time` in its syntax error.
/// let error = DateTimeError::InvalidInputSyntax(PgType::Time);
/// assert_eq!(error.to_string(), "invalid input syntax for type time");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PgType {
    /// `boolean`.
    Boolean,
    /// `smallint`.
    Smallint,
    /// `integer`.
    Integer,
    /// `bigint`.
    Bigint,
    /// `numeric`.
    Numeric,
    /// `text`.
    Text,
    /// `character varying`.
    Varchar,
    /// `character`.
    Bpchar,
    /// `date`.
    Date,
    /// `timestamp without time zone`.
    Timestamp,
    /// `timestamp with time zone`.
    TimestampTz,
    /// `time without time zone`.
    Time,
    /// `time with time zone`.
    TimeTz,
}

impl fmt::Display for PgType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl PgType {
    /// The name `format_type` gives the type.
    const fn name(self) -> &'static str {
        match self {
            Self::Boolean => "boolean",
            Self::Smallint => "smallint",
            Self::Integer => "integer",
            Self::Bigint => "bigint",
            Self::Numeric => "numeric",
            Self::Text => "text",
            Self::Varchar => "character varying",
            Self::Bpchar => "character",
            Self::Date => "date",
            Self::Timestamp => "timestamp without time zone",
            Self::TimestampTz => "timestamp with time zone",
            Self::Time => "time without time zone",
            Self::TimeTz => "time with time zone",
        }
    }

    /// The name the type's input function writes in its syntax error, which for `timestamp`
    /// and `time` leaves out `without time zone`.
    pub(crate) const fn input_name(self) -> &'static str {
        match self {
            Self::Timestamp => "timestamp",
            Self::Time => "time",
            other => other.name(),
        }
    }
}

impl CastTarget {
    /// The type without its modifier.
    #[must_use]
    pub const fn pg_type(self) -> PgType {
        match self {
            Self::Boolean => PgType::Boolean,
            Self::Smallint => PgType::Smallint,
            Self::Integer => PgType::Integer,
            Self::Bigint => PgType::Bigint,
            Self::Numeric(_) => PgType::Numeric,
            Self::Text => PgType::Text,
            Self::Varchar(_) => PgType::Varchar,
            Self::Bpchar(_) => PgType::Bpchar,
            Self::Date => PgType::Date,
            Self::Timestamp(_) => PgType::Timestamp,
            Self::TimestampTz(_) => PgType::TimestampTz,
        }
    }
}

/// The value a cast produces or takes, printed by [`Display`](fmt::Display) as the type's output
/// function prints it under the default `DateStyle` of `ISO`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CastValue {
    /// A `boolean`, printed `t` or `f`.
    Boolean(bool),
    /// A `smallint`.
    Smallint(i16),
    /// An `integer`.
    Integer(i32),
    /// A `bigint`.
    Bigint(i64),
    /// A `numeric`.
    Numeric(PgNumeric),
    /// A `text` or `varchar`.
    Text(String),
    /// A `char(n)`, blank-padded to its length as PostgreSQL holds it, equal to another as
    /// PostgreSQL finds it, ignoring trailing blanks.
    Bpchar(PgBpchar<'static>),
    /// A `date`.
    Date(PgDate),
    /// A `timestamp`.
    Timestamp(PgTimestamp),
    /// A `timestamptz`, printed as under `TimeZone` `UTC`.
    TimestampTz(PgTimestampTz),
}

/// Why PostgreSQL refuses a cast, one variant per message it raises, or why this crate declines
/// to answer for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, thiserror::Error)]
#[non_exhaustive]
pub enum CastError {
    /// Text the target's input function cannot read.
    #[error("invalid input syntax for type {}", .0.input_name())]
    InvalidInputSyntax(PgType),
    /// Text the integer input function reads as a number beyond the type.
    #[error("value is out of range for type {0}")]
    ValueOutOfRange(PgType),
    /// A value the cast to an integer type finds beyond the type.
    #[error("{0} out of range")]
    OutOfRange(PgType),
    /// A `NaN` cast to an integer type.
    #[error("cannot convert NaN to {0}")]
    CannotConvertNaN(PgType),
    /// An infinite `numeric` cast to an integer type.
    #[error("cannot convert infinity to {0}")]
    CannotConvertInfinity(PgType),
    /// A value beyond the precision of `numeric(p, s)` after rounding to its scale, or an
    /// infinity cast to it.
    #[error("numeric field overflow")]
    NumericFieldOverflow,
    /// A `numeric` beyond the weight or display scale PostgreSQL stores.
    #[error("value overflows numeric format")]
    NumericFormatOverflow,
    /// A year, month or day no calendar has.
    #[error("date/time field value out of range")]
    DateTimeFieldOutOfRange,
    /// A date before 4714-11-24 BC.
    #[error("date out of range")]
    DateOutOfRange,
    /// A timestamp before 4714-11-24 BC.
    #[error("timestamp out of range")]
    TimestampOutOfRange,
    /// A date from 294277-01-01 cast to `timestamp`.
    #[error("date out of range for timestamp")]
    DateOutOfRangeForTimestamp,
    /// An offset of 16 hours or more, or with minutes or seconds of 60 or more.
    #[error("time zone displacement out of range")]
    TimeZoneDisplacementOutOfRange,
    /// A value of a type with no cast to the target.
    #[error("cannot cast type {from} to {to}")]
    CannotCast {
        /// The value's type.
        from: PgType,
        /// The target's type.
        to: PgType,
    },
    /// A literal or cast this crate does not model. A date or timestamp is read only in the
    /// spellings [`PgDate::parse`] and [`PgTimestamp::parse`] read, and a `timestamptz` only
    /// with its offset, since without one the session's `TimeZone` decides the instant. For the
    /// same reason no cast between `timestamptz` and `date`, `timestamp` or a character type is
    /// modelled.
    #[error("cast not modelled")]
    Unsupported,
}

/// The value PostgreSQL 18's explicit cast `literal::target` produces, or the error it raises,
/// with a database encoding of UTF-8.
///
/// # Errors
///
/// The [`CastError`] PostgreSQL raises, or [`CastError::Unsupported`] for a literal or cast this
/// crate does not model.
///
/// ```
/// use sql_values::{CastError, CastTarget, CharLength, Literal, NumericTypmod, PgType, cast_literal};
///
/// let cast = |literal, target| cast_literal(literal, target).map(|value| value.to_string());
/// let length = |n| CharLength::new(n);
///
/// assert_eq!(cast(Literal::Number("2.5"), CastTarget::Integer).unwrap(), "3");
/// assert_eq!(cast(Literal::Number("-2.5"), CastTarget::Integer).unwrap(), "-3");
/// assert_eq!(cast(Literal::Number("1e2"), CastTarget::Integer).unwrap(), "100");
/// assert_eq!(cast(Literal::Number("2.5"), CastTarget::Smallint).unwrap(), "3");
/// assert_eq!(
///     cast(Literal::Number("32768"), CastTarget::Smallint),
///     Err(CastError::OutOfRange(PgType::Smallint)),
/// );
/// assert_eq!(
///     cast(Literal::Quoted("2.5"), CastTarget::Integer),
///     Err(CastError::InvalidInputSyntax(PgType::Integer)),
/// );
///
/// assert_eq!(cast(Literal::Quoted("abc"), CastTarget::Bpchar(length(2))).unwrap(), "ab");
/// assert_eq!(cast(Literal::Quoted("ab"), CastTarget::Bpchar(length(4))).unwrap(), "ab  ");
/// assert_eq!(cast(Literal::Quoted("ab"), CastTarget::Varchar(length(1))).unwrap(), "a");
/// assert_eq!(cast(Literal::Quoted("abc  "), CastTarget::Varchar(length(3))).unwrap(), "abc");
///
/// let numeric_4_1 = CastTarget::Numeric(NumericTypmod::new(4, 1));
/// assert_eq!(cast(Literal::Quoted("12.345"), numeric_4_1).unwrap(), "12.3");
/// assert_eq!(cast(Literal::Number("12.345"), numeric_4_1).unwrap(), "12.3");
///
/// assert_eq!(cast(Literal::Quoted("yes"), CastTarget::Boolean).unwrap(), "t");
///
/// assert_eq!(cast(Literal::Quoted("2026-01-01 23:00"), CastTarget::Date).unwrap(), "2026-01-01");
/// assert_eq!(
///     cast(Literal::Quoted("2026-01-01T23:00:00+05"), CastTarget::Date).unwrap(),
///     "2026-01-01",
/// );
/// assert_eq!(
///     cast(Literal::Quoted("2026-01-01 23:00:00.6"), CastTarget::Timestamp(Some(0))).unwrap(),
///     "2026-01-01 23:00:01",
/// );
/// assert_eq!(cast(Literal::Quoted("today"), CastTarget::Date), Err(CastError::Unsupported));
///
/// let instant = CastTarget::TimestampTz(None);
/// assert_eq!(
///     cast(Literal::Quoted("2030-01-01 00:00:00+02"), instant).unwrap(),
///     "2029-12-31 22:00:00+00",
/// );
/// assert_eq!(
///     cast(Literal::Quoted("2030-01-01 00:00:00"), instant),
///     Err(CastError::Unsupported),
/// );
/// ```
pub fn cast_literal(literal: Literal<'_>, target: CastTarget) -> Result<CastValue, CastError> {
    match literal {
        Literal::Quoted(text) => cast_quoted(text, target),
        Literal::Number(text) => cast_value(number_constant(text)?, target),
    }
}

/// The value PostgreSQL 18 makes of a numeric constant, written as [`Literal::Number`] takes it,
/// as its lexer and `make_const` type it. An integer is an `integer` when it fits, else a
/// `bigint` when it fits, else a `numeric`, and a constant with a point or an exponent is a
/// `numeric`.
///
/// # Errors
///
/// [`CastError::Unsupported`] for text that is not one numeric constant, and the error
/// `numeric_in` raises for a `numeric` PostgreSQL cannot store.
///
/// ```
/// use sql_values::{CastValue, number_constant};
///
/// assert_eq!(number_constant("2147483647"), Ok(CastValue::Integer(i32::MAX)));
/// assert_eq!(number_constant("2147483648"), Ok(CastValue::Bigint(2_147_483_648)));
/// assert_eq!(number_constant("-0x8000_0000"), Ok(CastValue::Integer(i32::MIN)));
/// let big = number_constant("9223372036854775808").unwrap();
/// assert!(matches!(big, CastValue::Numeric(_)));
/// assert_eq!(number_constant("1.50").unwrap().to_string(), "1.50");
/// ```
pub fn number_constant(text: &str) -> Result<CastValue, CastError> {
    let unsigned = text.strip_prefix('-').unwrap_or(text);
    match scan_number(unsigned.as_bytes()) {
        None => Err(CastError::Unsupported),
        Some(NumberToken::Integer) => [IntegerType::Integer, IntegerType::Bigint]
            .into_iter()
            .find_map(|integer| int_input(text, integer).ok())
            .map_or_else(|| numeric_input(text).map(CastValue::Numeric), Ok),
        Some(NumberToken::Decimal) => numeric_input(text).map(CastValue::Numeric),
    }
}

/// The value PostgreSQL 18's explicit cast `value::target` produces, or the error it raises.
///
/// A value of a character type is read by the target's input function, as `pg_cast`'s I/O
/// coercion does, and a value cast to a character type is printed by its type's output function,
/// `boolean` as `true` or `false`. Between the other types the cast is the function `pg_cast`
/// names, `int4_bool`, `bool_int4`, the integer narrowings, `int*_numeric`, `numeric_int*`,
/// `date_timestamp` and `timestamp_date`. The length coercion of the target's modifier follows,
/// and a pair `pg_cast` has no cast for raises [`CastError::CannotCast`].
///
/// # Errors
///
/// The [`CastError`] PostgreSQL raises, or [`CastError::Unsupported`] for a cast whose answer
/// the session's `TimeZone` decides, or a text the target's input function is not modelled for.
///
/// ```
/// use sql_values::{CastError, CastTarget, CastValue, PgNumeric, PgType, cast_value};
///
/// let cast = |value, target| cast_value(value, target).map(|value| value.to_string());
///
/// assert_eq!(cast(CastValue::Integer(1), CastTarget::Boolean).unwrap(), "t");
/// assert_eq!(
///     cast(CastValue::Bigint(1), CastTarget::Boolean),
///     Err(CastError::CannotCast { from: PgType::Bigint, to: PgType::Boolean }),
/// );
/// assert_eq!(cast(CastValue::Boolean(true), CastTarget::Text).unwrap(), "true");
/// let half = PgNumeric::parse("-2.5").unwrap();
/// assert_eq!(cast(CastValue::Numeric(half), CastTarget::Smallint).unwrap(), "-3");
/// assert_eq!(
///     cast(CastValue::Numeric(PgNumeric::NAN), CastTarget::Integer),
///     Err(CastError::CannotConvertNaN(PgType::Integer)),
/// );
/// assert_eq!(
///     cast(CastValue::Integer(40_000), CastTarget::Smallint),
///     Err(CastError::OutOfRange(PgType::Smallint)),
/// );
/// assert_eq!(
///     cast(CastValue::Text("x".into()), CastTarget::Boolean),
///     Err(CastError::InvalidInputSyntax(PgType::Boolean)),
/// );
/// ```
pub fn cast_value(value: CastValue, target: CastTarget) -> Result<CastValue, CastError> {
    let cannot_cast = CastError::CannotCast {
        from: value.pg_type(),
        to: target.pg_type(),
    };
    match (value, target) {
        (CastValue::Text(text), _) => cast_quoted(&text, target),
        // `rtrim1`, dropping the blanks `char(n)` pads with.
        (CastValue::Bpchar(value), CastTarget::Text | CastTarget::Varchar(_)) => {
            Ok(character(value.as_text(), target))
        }
        (CastValue::Bpchar(value), _) => cast_quoted(value.as_str(), target),
        (
            CastValue::TimestampTz(_),
            CastTarget::Date
            | CastTarget::Timestamp(_)
            | CastTarget::Text
            | CastTarget::Varchar(_)
            | CastTarget::Bpchar(_),
        )
        | (CastValue::Date(_) | CastValue::Timestamp(_), CastTarget::TimestampTz(_)) => {
            Err(CastError::Unsupported)
        }
        // `booltext`.
        (
            CastValue::Boolean(value),
            CastTarget::Text | CastTarget::Varchar(_) | CastTarget::Bpchar(_),
        ) => Ok(character(if value { "true" } else { "false" }, target)),
        (value, CastTarget::Text | CastTarget::Varchar(_) | CastTarget::Bpchar(_)) => {
            Ok(character(&value.to_string(), target))
        }
        (CastValue::Boolean(value), CastTarget::Boolean) => Ok(CastValue::Boolean(value)),
        (CastValue::Boolean(value), CastTarget::Integer) => Ok(CastValue::Integer(value.into())),
        (CastValue::Integer(value), CastTarget::Boolean) => Ok(CastValue::Boolean(value != 0)),
        (CastValue::Smallint(value), _) if target.takes_integer() => integer(value.into(), target),
        (CastValue::Integer(value), _) if target.takes_integer() => integer(value.into(), target),
        (CastValue::Bigint(value), _) if target.takes_integer() => integer(value, target),
        (CastValue::Numeric(value), CastTarget::Numeric(typmod)) => numeric_typmod(value, typmod),
        (CastValue::Numeric(value), _) if target.takes_integer() => {
            let to = target.pg_type();
            if value.is_nan() {
                Err(CastError::CannotConvertNaN(to))
            } else if value.is_infinite() {
                Err(CastError::CannotConvertInfinity(to))
            } else {
                let value = value.into_i64_rounded().ok_or(CastError::OutOfRange(to))?;
                integer(value, target)
            }
        }
        (CastValue::Date(date), CastTarget::Date) => Ok(CastValue::Date(date)),
        (CastValue::Date(date), CastTarget::Timestamp(precision)) => {
            // `date2timestamp` raises only this error.
            let at = date
                .to_timestamp()
                .map_err(|_| CastError::DateOutOfRangeForTimestamp)?;
            Ok(timestamp(at, precision))
        }
        (CastValue::Timestamp(at), CastTarget::Date) => Ok(CastValue::Date(at.date())),
        (CastValue::Timestamp(at), CastTarget::Timestamp(precision)) => {
            Ok(timestamp(at, precision))
        }
        (CastValue::TimestampTz(at), CastTarget::TimestampTz(precision)) => {
            let micros = adjust_timestamp_for_typmod(at.pg_micros(), precision);
            Ok(CastValue::TimestampTz(PgTimestampTz::from_stored(micros)))
        }
        _ => Err(cannot_cast),
    }
}

impl CastTarget {
    /// Whether the target is an integer type or `numeric`, which every integer type casts to.
    const fn takes_integer(self) -> bool {
        matches!(
            self,
            Self::Smallint | Self::Integer | Self::Bigint | Self::Numeric(_)
        )
    }
}

impl CastValue {
    /// The value's type, `text` for a `varchar`.
    const fn pg_type(&self) -> PgType {
        match self {
            Self::Boolean(_) => PgType::Boolean,
            Self::Smallint(_) => PgType::Smallint,
            Self::Integer(_) => PgType::Integer,
            Self::Bigint(_) => PgType::Bigint,
            Self::Numeric(_) => PgType::Numeric,
            Self::Text(_) => PgType::Text,
            Self::Bpchar(_) => PgType::Bpchar,
            Self::Date(_) => PgType::Date,
            Self::Timestamp(_) => PgType::Timestamp,
            Self::TimestampTz(_) => PgType::TimestampTz,
        }
    }
}

/// The integer narrowings `int82`, `int84` and `int42`, or `int*_numeric` and the `numeric()`
/// length coercion, of an integer to a target that [`CastTarget::takes_integer`].
fn integer(value: i64, target: CastTarget) -> Result<CastValue, CastError> {
    match IntegerType::try_from(target) {
        Ok(integer) => integer.value(value),
        Err(CastTarget::Numeric(typmod)) => numeric_typmod(PgNumeric::from(value), typmod),
        Err(other) => unreachable!("{other:?} takes no integer"),
    }
}

/// `timestamp_scale`, the timestamp rounded to the target's precision.
fn timestamp(at: PgTimestamp, precision: Option<u8>) -> CastValue {
    let micros = adjust_timestamp_for_typmod(at.pg_micros(), precision);
    CastValue::Timestamp(PgTimestamp::from_stored(micros))
}

/// `numeric_in`, its refusal as the cast error. It raises only a syntax error or an overflow.
fn numeric_input(text: &str) -> Result<PgNumeric, CastError> {
    PgNumeric::parse(text).map_err(|error| {
        if error == NumericError::InvalidInputSyntax {
            CastError::InvalidInputSyntax(PgType::Numeric)
        } else {
            CastError::NumericFormatOverflow
        }
    })
}

/// The lexer's token for a numeric constant.
enum NumberToken {
    /// `decinteger`, `hexinteger`, `octinteger` or `bininteger`.
    Integer,
    /// `numeric` or `real`.
    Decimal,
}

/// The token PostgreSQL 18's `scan.l` reads `bytes` as, or `None` when it is not one numeric
/// constant.
fn scan_number(bytes: &[u8]) -> Option<NumberToken> {
    let (radix, digits) = match bytes {
        [b'0', b'x' | b'X', digits @ ..] => (16, digits),
        [b'0', b'o' | b'O', digits @ ..] => (8, digits),
        [b'0', b'b' | b'B', digits @ ..] => (2, digits),
        _ => return scan_decimal(bytes),
    };
    // `0[xX](_?{hexdigit})+`, where an underscore may follow the prefix.
    let mut at = 0;
    while at < digits.len() {
        at += usize::from(digits[at] == b'_');
        if !digits
            .get(at)
            .is_some_and(|&b| char::from(b).is_digit(radix))
        {
            return None;
        }
        at += 1;
    }
    (at > 0).then_some(NumberToken::Integer)
}

/// `decinteger`, `numeric` or `real`.
fn scan_decimal(bytes: &[u8]) -> Option<NumberToken> {
    let integer = scan_decinteger(bytes);
    let mut at = integer;
    let mut token = NumberToken::Integer;
    if bytes.get(at) == Some(&b'.') {
        token = NumberToken::Decimal;
        at += 1;
        let fraction = scan_decinteger(&bytes[at..]);
        if integer == 0 && fraction == 0 {
            return None;
        }
        at += fraction;
    } else if integer == 0 {
        return None;
    }
    if matches!(bytes.get(at), Some(b'e' | b'E')) {
        token = NumberToken::Decimal;
        at += 1;
        at += usize::from(matches!(bytes.get(at), Some(b'+' | b'-')));
        let exponent = scan_decinteger(&bytes[at..]);
        if exponent == 0 {
            return None;
        }
        at += exponent;
    }
    (at == bytes.len()).then_some(token)
}

/// The length of the leading `{decdigit}(_?{decdigit})*`, `0` when there is none.
fn scan_decinteger(bytes: &[u8]) -> usize {
    if !bytes.first().is_some_and(u8::is_ascii_digit) {
        return 0;
    }
    let mut at = 1;
    loop {
        let next = at + usize::from(bytes.get(at) == Some(&b'_'));
        if !bytes.get(next).is_some_and(u8::is_ascii_digit) {
            return at;
        }
        at = next + 1;
    }
}

/// The cast of an unknown literal, the target's input function without a modifier followed by
/// the length coercion.
fn cast_quoted(text: &str, target: CastTarget) -> Result<CastValue, CastError> {
    // A string constant never holds NUL, which ends a C string.
    if text.contains('\0') {
        return Err(CastError::Unsupported);
    }
    match target {
        CastTarget::Boolean => bool_in(text).map(CastValue::Boolean),
        CastTarget::Smallint => int_input(text, IntegerType::Smallint),
        CastTarget::Integer => int_input(text, IntegerType::Integer),
        CastTarget::Bigint => int_input(text, IntegerType::Bigint),
        CastTarget::Numeric(typmod) => numeric_typmod(numeric_input(text)?, typmod),
        CastTarget::Text | CastTarget::Varchar(_) | CastTarget::Bpchar(_) => {
            Ok(character(text, target))
        }
        CastTarget::Date => PgDate::parse(text, InputContext::new())
            .map(CastValue::Date)
            .map_err(datetime_error),
        CastTarget::Timestamp(precision) => {
            let at = PgTimestamp::parse(text, InputContext::new()).map_err(datetime_error)?;
            let micros = adjust_timestamp_for_typmod(at.pg_micros(), precision);
            Ok(CastValue::Timestamp(PgTimestamp::from_stored(micros)))
        }
        CastTarget::TimestampTz(precision) => {
            let at = PgTimestampTz::parse(text, InputContext::new()).map_err(datetime_error)?;
            let micros = adjust_timestamp_for_typmod(at.pg_micros(), precision);
            Ok(CastValue::TimestampTz(PgTimestampTz::from_stored(micros)))
        }
    }
}

fn numeric_typmod(value: PgNumeric, typmod: Option<NumericTypmod>) -> Result<CastValue, CastError> {
    match typmod {
        Some(typmod) => value.apply_typmod(typmod),
        None => Ok(value),
    }
    .map(CastValue::Numeric)
}

/// The `varchar()` or `bpchar()` length coercion of an explicit cast, which truncates to the
/// length and, for `char(n)`, pads with blanks.
fn character(text: &str, target: CastTarget) -> CastValue {
    match target {
        CastTarget::Varchar(Some(length)) => CastValue::Text(clip(text, length).into()),
        CastTarget::Bpchar(Some(length)) => {
            let clipped = clip(text, length);
            let mut padded = String::from(clipped);
            let blanks = length.0 - clipped.chars().count();
            padded.extend(core::iter::repeat_n(' ', blanks));
            CastValue::Bpchar(PgBpchar::new(padded))
        }
        CastTarget::Bpchar(None) => CastValue::Bpchar(PgBpchar::new(String::from(text))),
        _ => CastValue::Text(text.into()),
    }
}

/// The first `length` characters of `text`.
fn clip(text: &str, length: CharLength) -> &str {
    text.char_indices()
        .nth(length.0)
        .map_or(text, |(end, _)| &text[..end])
}

/// The C library's `isspace` in the C locale.
pub(crate) const fn is_pg_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | 0x0B | 0x0C | b'\r')
}

pub(crate) fn trim_pg_space(bytes: &[u8]) -> &[u8] {
    let start = bytes.iter().take_while(|&&b| is_pg_space(b)).count();
    let end = bytes
        .iter()
        .rposition(|&b| !is_pg_space(b))
        .map_or(start, |last| last + 1);
    &bytes[start..end.max(start)]
}

/// `boolin`, reading a case-insensitive prefix of `true`, `false`, `yes` or `no`, `on`, `of` or
/// `off`, or `1` or `0`, between blanks.
fn bool_in(text: &str) -> Result<bool, CastError> {
    let word = trim_pg_space(text.as_bytes());
    let abbreviates = |full: &[u8]| {
        full.get(..word.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(word))
    };
    let value = match word {
        [b't' | b'T', ..] if abbreviates(b"true") => true,
        [b'f' | b'F', ..] if abbreviates(b"false") => false,
        [b'y' | b'Y', ..] if abbreviates(b"yes") => true,
        [b'n' | b'N', ..] if abbreviates(b"no") => false,
        [b'o' | b'O', _, ..] if abbreviates(b"on") => true,
        [b'o' | b'O', _, ..] if abbreviates(b"off") => false,
        b"1" => true,
        b"0" => false,
        _ => return Err(CastError::InvalidInputSyntax(PgType::Boolean)),
    };
    Ok(value)
}

/// Why `pg_strtoint*` refuses text.
enum IntInputError {
    Syntax,
    Range,
}

/// `int_in` read as `integer`, mapping its refusal to the error naming that type.
fn int_input(text: &str, integer: IntegerType) -> Result<CastValue, CastError> {
    let value = int_in(text, integer).map_err(|error| match error {
        IntInputError::Syntax => CastError::InvalidInputSyntax(integer.into()),
        IntInputError::Range => CastError::ValueOutOfRange(integer.into()),
    })?;
    Ok(integer
        .value(value)
        .expect("int_in kept the value in the type's range"))
}

/// `pg_strtoint16`, `pg_strtoint32` or `pg_strtoint64` by `integer`, reading blanks, a sign, a
/// decimal, `0x`, `0o` or `0b` integer with single underscores between digits, then blanks. An
/// overflow stops the read where it happens, before any trailing junk is seen.
fn int_in(text: &str, integer: IntegerType) -> Result<i64, IntInputError> {
    let bytes = text.as_bytes();
    let mut at = bytes.iter().take_while(|&&b| is_pg_space(b)).count();
    let negative = bytes.get(at) == Some(&b'-');
    at += usize::from(matches!(bytes.get(at), Some(b'+' | b'-')));
    let radix = match (bytes.get(at), bytes.get(at + 1)) {
        (Some(b'0'), Some(b'x' | b'X')) => 16,
        (Some(b'0'), Some(b'o' | b'O')) => 8,
        (Some(b'0'), Some(b'b' | b'B')) => 2,
        _ => 10,
    };
    if radix != 10 {
        at += 2;
    }
    let first_digit = at;
    let is_digit = |byte: Option<&u8>| byte.is_some_and(|&b| char::from(b).is_digit(radix));
    // The magnitude of the type's minimum, which a negative value reaches.
    let limit = integer.range().start().unsigned_abs();
    let mut magnitude: u64 = 0;
    loop {
        match bytes.get(at) {
            Some(&byte) if is_digit(Some(&byte)) => {
                if magnitude > limit / u64::from(radix) {
                    return Err(IntInputError::Range);
                }
                let digit = char::from(byte).to_digit(radix).expect("checked a digit");
                magnitude = magnitude * u64::from(radix) + u64::from(digit);
                at += 1;
            }
            Some(b'_') => {
                if radix == 10 && at == first_digit {
                    return Err(IntInputError::Syntax);
                }
                at += 1;
                if !is_digit(bytes.get(at)) {
                    return Err(IntInputError::Syntax);
                }
            }
            _ => break,
        }
    }
    if at == first_digit || !bytes[at..].iter().all(|&b| is_pg_space(b)) {
        return Err(IntInputError::Syntax);
    }
    let value = if negative {
        -i128::from(magnitude)
    } else {
        i128::from(magnitude)
    };
    i64::try_from(value)
        .ok()
        .filter(|value| integer.range().contains(value))
        .ok_or(IntInputError::Range)
}

/// `AdjustTimestampForTypmod`, rounding a finite timestamp half away from 2000-01-01 to
/// `precision` digits, with no range check after, as PostgreSQL rounds the last second of
/// 294276 up to 294277-01-01.
fn adjust_timestamp_for_typmod(micros: i64, precision: Option<u8>) -> i64 {
    let Some(precision @ 0..6) = precision else {
        return micros;
    };
    if micros == i64::MAX || micros == i64::MIN {
        return micros;
    }
    let scale = 10i64.pow(6 - u32::from(precision));
    let offset = scale / 2;
    if micros >= 0 {
        (micros + offset) / scale * scale
    } else {
        -((-micros + offset) / scale * scale)
    }
}

/// The cast error of a refusal by a date or time input function.
fn datetime_error(error: DateTimeError) -> CastError {
    match error {
        DateTimeError::FieldValueOutOfRange => CastError::DateTimeFieldOutOfRange,
        DateTimeError::TimeZoneDisplacementOutOfRange => CastError::TimeZoneDisplacementOutOfRange,
        DateTimeError::DateOutOfRange => CastError::DateOutOfRange,
        DateTimeError::TimestampOutOfRange => CastError::TimestampOutOfRange,

        DateTimeError::InvalidInputSyntax(pg_type) => CastError::InvalidInputSyntax(pg_type),
        DateTimeError::OutsideJiffRange
        | DateTimeError::NeedsTimeZone
        | DateTimeError::NeedsDateOrder
        | DateTimeError::NeedsCurrentTime
        | DateTimeError::TimeZoneNotRecognized => CastError::Unsupported,
    }
}

/// Each type's output function under `DateStyle` `ISO` and `TimeZone` `UTC`.
impl fmt::Display for CastValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Boolean(value) => f.write_str(if *value { "t" } else { "f" }),
            Self::Smallint(value) => write!(f, "{value}"),
            Self::Integer(value) => write!(f, "{value}"),
            Self::Bigint(value) => write!(f, "{value}"),
            Self::Numeric(value) => write!(f, "{value}"),
            Self::Text(text) => f.write_str(text),
            Self::Bpchar(value) => write!(f, "{value}"),
            Self::Date(date) => write!(f, "{date}"),
            Self::Timestamp(at) => write!(f, "{at}"),
            Self::TimestampTz(at) => write!(f, "{at}"),
        }
    }
}
