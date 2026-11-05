//! PostgreSQL 18's explicit cast of a literal, `'…'::type` or `1.5::type`, as the input function
//! or the cast from the constant's type followed by the length coercion of the target's modifier.

use alloc::string::{String, ToString};
use core::fmt;

use chrono::{DateTime, Datelike, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime, Timelike, Utc};

use crate::{
    interval::{ShiftError, chrono_timestamp, pg_microseconds, pg_timestamp, valid_timestamp},
    numeric::{NumericTypmod, PgNumeric},
    parse_pg_offset, parse_two_digits,
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

/// The type a literal is cast to, with its modifier.
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

/// A type PostgreSQL names in a cast's error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
}

impl fmt::Display for PgType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
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
        })
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

/// The value a cast produces, printed by [`Display`](fmt::Display) as the type's output function
/// prints it under the default `DateStyle` of `ISO`.
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
    /// A `char(n)`, blank-padded to its length as PostgreSQL holds it. PostgreSQL drops the
    /// trailing blanks when it compares it with `text` or casts it to `text`.
    Bpchar(String),
    /// A `date`, where year `Y` BC is year `1 - Y`.
    Date(NaiveDate),
    /// A `timestamp`, where year `Y` BC is year `1 - Y`.
    Timestamp(NaiveDateTime),
    /// A `timestamptz`, the instant PostgreSQL holds, printed as under `TimeZone` `UTC`.
    TimestampTz(DateTime<Utc>),
}

/// Why PostgreSQL refuses a cast, one variant per message it raises, or why this crate declines
/// to answer for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, thiserror::Error)]
pub enum CastError {
    /// Text the target's input function cannot read.
    #[error("invalid input syntax for type {0}")]
    InvalidInputSyntax(PgType),
    /// Text the integer input function reads as a number beyond the type.
    #[error("value is out of range for type {0}")]
    ValueOutOfRange(PgType),
    /// A constant the cast to an integer type finds beyond the type.
    #[error("{0} out of range")]
    OutOfRange(PgType),
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
    /// A numeric constant of a type with no cast to the target.
    #[error("cannot cast type {from} to {to}")]
    CannotCast {
        /// The constant's type.
        from: PgType,
        /// The target's type.
        to: PgType,
    },
    /// A literal or cast this crate does not model. A date or timestamp is read only in the
    /// ISO spelling `YYYY-MM-DD[ HH:MM[:SS[.ffffff]][offset]][ BC]`, the one no session setting
    /// changes, and a `timestamptz` only with its offset, since without one the session's
    /// `TimeZone` decides the instant.
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
        Literal::Number(text) => cast_constant(Constant::read(text)?, target),
    }
}

/// A numeric constant as `make_const` types it.
enum Constant {
    Integer(i32),
    Bigint(i64),
    Numeric(PgNumeric),
}

impl Constant {
    /// The constant PostgreSQL's lexer and `make_const` make of `text`.
    fn read(text: &str) -> Result<Self, CastError> {
        let unsigned = text.strip_prefix('-').unwrap_or(text);
        match scan_number(unsigned.as_bytes()) {
            None => Err(CastError::Unsupported),
            Some(NumberToken::Integer) => {
                if let Ok(value) = int_in(text, 32) {
                    Ok(Self::Integer(narrow(value)))
                } else if let Ok(value) = int_in(text, 64) {
                    Ok(Self::Bigint(value))
                } else {
                    PgNumeric::parse(text).map(Self::Numeric)
                }
            }
            Some(NumberToken::Decimal) => PgNumeric::parse(text).map(Self::Numeric),
        }
    }

    const fn pg_type(&self) -> PgType {
        match self {
            Self::Integer(_) => PgType::Integer,
            Self::Bigint(_) => PgType::Bigint,
            Self::Numeric(_) => PgType::Numeric,
        }
    }

    /// The constant as an `i64`, `numeric` rounded half away from zero, or `None` beyond it.
    fn into_i64(self) -> Option<i64> {
        match self {
            Self::Integer(value) => Some(i64::from(value)),
            Self::Bigint(value) => Some(value),
            Self::Numeric(value) => value.into_i64_rounded(),
        }
    }
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

/// The cast of a numeric constant by `int4_bool`, the integer narrowings, `numeric_int*`, the
/// `numeric()` length coercion, or the output function for a character type.
fn cast_constant(constant: Constant, target: CastTarget) -> Result<CastValue, CastError> {
    let from = constant.pg_type();
    match target {
        CastTarget::Boolean => match constant {
            Constant::Integer(value) => Ok(CastValue::Boolean(value != 0)),
            _ => Err(CastError::CannotCast {
                from,
                to: PgType::Boolean,
            }),
        },
        CastTarget::Smallint => {
            let value = constant.into_i64().and_then(|v| i16::try_from(v).ok());
            value
                .map(CastValue::Smallint)
                .ok_or(CastError::OutOfRange(PgType::Smallint))
        }
        CastTarget::Integer => {
            let value = constant.into_i64().and_then(|v| i32::try_from(v).ok());
            value
                .map(CastValue::Integer)
                .ok_or(CastError::OutOfRange(PgType::Integer))
        }
        CastTarget::Bigint => constant
            .into_i64()
            .map(CastValue::Bigint)
            .ok_or(CastError::OutOfRange(PgType::Bigint)),
        CastTarget::Numeric(typmod) => {
            let value = match constant {
                Constant::Integer(value) => PgNumeric::from_i64(i64::from(value)),
                Constant::Bigint(value) => PgNumeric::from_i64(value),
                Constant::Numeric(value) => value,
            };
            numeric_typmod(value, typmod)
        }
        CastTarget::Text | CastTarget::Varchar(_) | CastTarget::Bpchar(_) => {
            let text = match constant {
                Constant::Integer(value) => value.to_string(),
                Constant::Bigint(value) => value.to_string(),
                Constant::Numeric(value) => value.to_string(),
            };
            Ok(character(&text, target))
        }
        CastTarget::Date | CastTarget::Timestamp(_) | CastTarget::TimestampTz(_) => {
            Err(CastError::CannotCast {
                from,
                to: target.pg_type(),
            })
        }
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
        CastTarget::Smallint => int_input(text, 16, PgType::Smallint).map(|v| {
            CastValue::Smallint(i16::try_from(v).expect("int_in kept the value in 16 bits"))
        }),
        CastTarget::Integer => {
            int_input(text, 32, PgType::Integer).map(|v| CastValue::Integer(narrow(v)))
        }
        CastTarget::Bigint => int_input(text, 64, PgType::Bigint).map(CastValue::Bigint),
        CastTarget::Numeric(typmod) => numeric_typmod(PgNumeric::parse(text)?, typmod),
        CastTarget::Text | CastTarget::Varchar(_) | CastTarget::Bpchar(_) => {
            Ok(character(text, target))
        }
        CastTarget::Date => {
            let literal = read_datetime(text)?;
            if (literal.year, literal.month, literal.day) < (-4713, 11, 24) {
                return Err(CastError::DateOutOfRange);
            }
            literal.date().map(CastValue::Date)
        }
        CastTarget::Timestamp(precision) => {
            let literal = read_datetime(text)?;
            if !literal.julian() {
                return Err(CastError::TimestampOutOfRange);
            }
            let local = literal.date()?.and_time(literal.time);
            let micros = pg_timestamp(local).map_err(shift_error)?;
            let micros = round_timestamp(micros, precision);
            chrono_timestamp(micros)
                .map(CastValue::Timestamp)
                .map_err(shift_error)
        }
        CastTarget::TimestampTz(precision) => {
            let literal = read_datetime(text)?;
            // Without an offset the instant depends on the session's `TimeZone`.
            let offset = literal.offset.ok_or(CastError::Unsupported)?;
            if !literal.julian() {
                return Err(CastError::TimestampOutOfRange);
            }
            let local = pg_microseconds(literal.date()?.and_time(literal.time));
            let utc = local - i64::from(offset.local_minus_utc()) * 1_000_000;
            let micros = valid_timestamp(utc).map_err(shift_error)?;
            let micros = round_timestamp(micros, precision);
            chrono_timestamp(micros)
                .map(|at| CastValue::TimestampTz(at.and_utc()))
                .map_err(shift_error)
        }
    }
}

/// An `i64` `int_in` read within 32 bits.
fn narrow(value: i64) -> i32 {
    i32::try_from(value).expect("int_in kept the value in 32 bits")
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
            CastValue::Bpchar(padded)
        }
        CastTarget::Bpchar(None) => CastValue::Bpchar(text.into()),
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

fn trim_pg_space(bytes: &[u8]) -> &[u8] {
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

/// `int_in` for a type of `bits`, mapping its refusal to the error naming `pg_type`.
fn int_input(text: &str, bits: u32, pg_type: PgType) -> Result<i64, CastError> {
    int_in(text, bits).map_err(|error| match error {
        IntInputError::Syntax => CastError::InvalidInputSyntax(pg_type),
        IntInputError::Range => CastError::ValueOutOfRange(pg_type),
    })
}

/// `pg_strtoint16`, `pg_strtoint32` or `pg_strtoint64` by `bits`, reading blanks, a sign, a
/// decimal, `0x`, `0o` or `0b` integer with single underscores between digits, then blanks. An
/// overflow stops the read where it happens, before any trailing junk is seen.
fn int_in(text: &str, bits: u32) -> Result<i64, IntInputError> {
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
    let limit: u64 = 1 << (bits - 1);
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
    let range = -i128::from(limit)..i128::from(limit);
    if range.contains(&value) {
        Ok(i64::try_from(value).expect("a value within 64 bits fits i64"))
    } else {
        Err(IntInputError::Range)
    }
}

/// A date and time literal in the one spelling [`read_datetime`] reads, its fields valid.
struct DateTimeLiteral {
    /// The proleptic Gregorian year, where year `Y` BC is `1 - Y`.
    year: i32,
    month: u8,
    day: u8,
    time: NaiveTime,
    offset: Option<FixedOffset>,
}

impl DateTimeLiteral {
    /// `IS_VALID_JULIAN`'s lower bound, which `date2j` checks only to the month.
    fn julian(&self) -> bool {
        (self.year, self.month) >= (-4713, 11)
    }

    /// The date, or [`CastError::Unsupported`] past `chrono`'s last year.
    fn date(&self) -> Result<NaiveDate, CastError> {
        NaiveDate::from_ymd_opt(self.year, self.month.into(), self.day.into())
            .ok_or(CastError::Unsupported)
    }
}

/// Read `YYYY-MM-DD[ HH:MM[:SS[.ffffff]][offset]][ BC]` between blanks, the spelling `date_in`,
/// `timestamp_in` and `timestamptz_in` read alike under every `DateStyle`.
fn read_datetime(text: &str) -> Result<DateTimeLiteral, CastError> {
    let unsupported = CastError::Unsupported;
    let trimmed = trim_pg_space(text.as_bytes());
    let (body, bc) = match trimmed.strip_suffix(b" BC") {
        Some(body) => (body, true),
        None => (trimmed, false),
    };
    let year_len = body.iter().take_while(|b| b.is_ascii_digit()).count();
    if !(4..=9).contains(&year_len) || (year_len > 4 && body[0] == b'0') {
        return Err(unsupported);
    }
    let (year_digits, rest) = body.split_at(year_len);
    let year = year_digits
        .iter()
        .fold(0i32, |year, &digit| year * 10 + i32::from(digit - b'0'));
    let Some((&[b'-', m1, m2, b'-', d1, d2], rest)) = rest.split_first_chunk() else {
        return Err(unsupported);
    };
    let month = parse_two_digits(&[m1, m2]).ok_or(unsupported)?;
    let day = parse_two_digits(&[d1, d2]).ok_or(unsupported)?;
    let (time, offset) = match rest.split_first() {
        None => (NaiveTime::MIN, None),
        Some((b' ' | b'T', clock)) => read_clock(clock).ok_or(unsupported)?,
        Some(_) => return Err(unsupported),
    };
    if year == 0 || !(1..=12).contains(&month) {
        return Err(CastError::DateTimeFieldOutOfRange);
    }
    let year = if bc { 1 - year } else { year };
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let month_days = match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if !(1..=month_days).contains(&day) {
        return Err(CastError::DateTimeFieldOutOfRange);
    }
    Ok(DateTimeLiteral {
        year,
        month,
        day,
        time,
        offset,
    })
}

/// Read `HH:MM[:SS[.ffffff]][offset]` within a day, or `None` past what [`read_datetime`] reads.
fn read_clock(bytes: &[u8]) -> Option<(NaiveTime, Option<FixedOffset>)> {
    let (&[h1, h2, b':', m1, m2], mut rest) = bytes.split_first_chunk()? else {
        return None;
    };
    let mut second = 0;
    let mut micros = 0;
    if let Some((&[b':', s1, s2], after)) = rest.split_first_chunk() {
        second = parse_two_digits(&[s1, s2])?;
        rest = after;
        if let Some((b'.', fraction)) = rest.split_first() {
            let len = fraction.iter().take_while(|b| b.is_ascii_digit()).count();
            if !(1..=6).contains(&len) {
                return None;
            }
            let (digits, after) = fraction.split_at(len);
            micros = digits
                .iter()
                .chain(core::iter::repeat_n(&b'0', 6 - len))
                .fold(0u32, |micros, &digit| micros * 10 + u32::from(digit - b'0'));
            rest = after;
        }
    }
    let offset = match rest {
        [] => None,
        // `DecodeTimezone` refuses an offset of 16 hours or more.
        _ => Some(parse_pg_offset(rest).filter(|o| o.local_minus_utc().abs() < 16 * 3600)?),
    };
    let time = NaiveTime::from_hms_micro_opt(
        parse_two_digits(&[h1, h2])?.into(),
        parse_two_digits(&[m1, m2])?.into(),
        second.into(),
        micros,
    )?;
    Some((time, offset))
}

/// `AdjustTimestampForTypmod`, rounding half away from 2000-01-01 to `precision` digits.
fn round_timestamp(micros: i64, precision: Option<u8>) -> i64 {
    let Some(precision @ 0..6) = precision else {
        return micros;
    };
    let scale = 10i64.pow(6 - u32::from(precision));
    let offset = scale / 2;
    if micros >= 0 {
        (micros + offset) / scale * scale
    } else {
        -((-micros + offset) / scale * scale)
    }
}

fn shift_error(error: ShiftError) -> CastError {
    match error {
        ShiftError::TimestampOutOfRange => CastError::TimestampOutOfRange,
        _ => CastError::Unsupported,
    }
}

/// PostgreSQL's ISO date, `Y BC` for year `1 - Y` with the era left to the caller.
fn write_date(f: &mut fmt::Formatter<'_>, date: NaiveDate) -> fmt::Result {
    let year = date.year();
    let shown = if year > 0 { year } else { 1 - year };
    write!(f, "{shown:04}-{:02}-{:02}", date.month(), date.day())
}

/// `EncodeDateTime` under `DateStyle` `ISO`, with `+00` for a `timestamptz` in `UTC`.
fn write_timestamp(f: &mut fmt::Formatter<'_>, at: NaiveDateTime, zone: bool) -> fmt::Result {
    write_date(f, at.date())?;
    let time = at.time();
    write!(
        f,
        " {:02}:{:02}:{:02}",
        time.hour(),
        time.minute(),
        time.second()
    )?;
    let mut micros = time.nanosecond() / 1_000;
    if micros > 0 {
        let mut width = 6;
        while micros % 10 == 0 {
            micros /= 10;
            width -= 1;
        }
        write!(f, ".{micros:0width$}")?;
    }
    if zone {
        f.write_str("+00")?;
    }
    if at.year() <= 0 {
        f.write_str(" BC")?;
    }
    Ok(())
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
            Self::Text(text) | Self::Bpchar(text) => f.write_str(text),
            Self::Date(date) => {
                write_date(f, *date)?;
                if date.year() <= 0 {
                    f.write_str(" BC")?;
                }
                Ok(())
            }
            Self::Timestamp(at) => write_timestamp(f, *at, false),
            Self::TimestampTz(at) => write_timestamp(f, at.naive_utc(), true),
        }
    }
}
