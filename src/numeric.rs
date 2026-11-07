//! PostgreSQL `numeric` values as `numeric_in` reads them, `numeric_out` prints them,
//! `cmp_numerics` orders them, the `numeric()` length coercion rounds them and the operators and
//! rounding functions compute them.

mod arithmetic;
mod magnitude;

use alloc::vec::Vec;
use core::{
    cmp::Ordering,
    fmt,
    hash::{Hash, Hasher},
};

use crate::cast::{CastError, is_pg_space};
use magnitude::Magnitude;

/// Why PostgreSQL refuses to read or compute a `numeric`, one variant per message it raises.
///
/// ```
/// use sql_values::{NumericError, PgNumeric};
///
/// assert_eq!(PgNumeric::parse("1.5x"), Err(NumericError::InvalidInputSyntax));
/// let one = PgNumeric::from(1);
/// let zero = PgNumeric::from(0);
/// assert_eq!(one.checked_div(&zero), Err(NumericError::DivisionByZero));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, thiserror::Error)]
pub enum NumericError {
    /// Text `numeric_in` cannot read.
    #[error("invalid input syntax for type numeric")]
    InvalidInputSyntax,
    /// A value beyond the weight or display scale PostgreSQL stores.
    #[error("value overflows numeric format")]
    FormatOverflow,
    /// A division or remainder by zero.
    #[error("division by zero")]
    DivisionByZero,
}

/// `NUMERIC_DSCALE_MAX`, the most digits after the point PostgreSQL stores.
const MAX_DSCALE: i64 = 0x3FFF;
/// The decimal weight of the leading digit past which PostgreSQL's base-10000 weight leaves
/// `int16`, `(32767 + 1) * 4 - 1`.
const MAX_WEIGHT: i64 = 131_071;
/// `PG_INT32_MAX / 2`, the exponent `set_var_from_str` refuses beyond.
const MAX_EXPONENT: i64 = 1_073_741_823;
/// `NUMERIC_MAX_PRECISION`.
const MAX_PRECISION: u16 = 1000;
/// `NUMERIC_MIN_SCALE` and `NUMERIC_MAX_SCALE`.
const SCALE_RANGE: core::ops::RangeInclusive<i16> = -1000..=1000;

/// A PostgreSQL `numeric`, equal, ordered and hashed by value as `cmp_numerics` compares it, with
/// `NaN` equal to itself and above `Infinity`.
///
/// Its [`Display`](fmt::Display) is `numeric_out`, so equal values can print differently.
///
/// ```
/// use sql_values::PgNumeric;
///
/// let numeric = |text| PgNumeric::parse(text).unwrap();
/// assert_eq!(numeric("1.50"), numeric("1.5"));
/// assert_eq!(numeric("1.50").to_string(), "1.50");
/// assert!(numeric("NaN") > numeric("Infinity"));
/// assert!(numeric("-inf") < numeric("-1e1000"));
/// assert_eq!(PgNumeric::from(-12_i64), numeric("-12.0"));
/// ```
#[derive(Debug, Clone)]
pub struct PgNumeric(Repr);

#[derive(Debug, Clone)]
enum Repr {
    NaN,
    Infinity { negative: bool },
    Finite(Finite),
}

/// A finite value as decimal digits, normalised so that equal values have equal fields except
/// `dscale`.
#[derive(Debug, Clone)]
struct Finite {
    /// Never set for zero.
    negative: bool,
    /// Digits `0..=9` with no leading or trailing zero, empty for zero.
    digits: Vec<u8>,
    /// The power of ten of `digits[0]`, `0` for zero.
    weight: i64,
    /// The digits printed after the point, never fewer than the value has.
    dscale: i64,
}

/// The modifier of a `numeric(p, s)` type, the precision and scale it rounds to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NumericTypmod {
    precision: u16,
    scale: i16,
}

impl NumericTypmod {
    /// `numerictypmodin`, or `None` for a precision outside `1..=1000` or a scale outside
    /// `-1000..=1000`. `numeric(p)` is a scale of `0`.
    ///
    /// ```
    /// use sql_values::NumericTypmod;
    ///
    /// assert!(NumericTypmod::new(4, 1).is_some());
    /// assert!(NumericTypmod::new(2, -3).is_some());
    /// assert!(NumericTypmod::new(0, 0).is_none());
    /// assert!(NumericTypmod::new(1001, 0).is_none());
    /// ```
    #[must_use]
    pub fn new(precision: u16, scale: i16) -> Option<Self> {
        ((1..=MAX_PRECISION).contains(&precision) && SCALE_RANGE.contains(&scale))
            .then_some(Self { precision, scale })
    }
}

impl PgNumeric {
    /// `NaN`, equal to itself and above every other value.
    pub const NAN: Self = Self(Repr::NaN);
    /// `Infinity`.
    pub const INFINITY: Self = Self(Repr::Infinity { negative: false });
    /// `-Infinity`.
    pub const NEG_INFINITY: Self = Self(Repr::Infinity { negative: true });

    /// PostgreSQL's `numeric_in` with no type modifier, reading blanks, a sign, a decimal number
    /// with an optional exponent, a `0x`, `0o` or `0b` integer, `NaN` or an infinity, then
    /// blanks.
    ///
    /// # Errors
    ///
    /// [`NumericError::InvalidInputSyntax`] for text `numeric_in` cannot read, and
    /// [`NumericError::FormatOverflow`] for a value beyond what PostgreSQL stores.
    ///
    /// ```
    /// use sql_values::{NumericError, PgNumeric};
    ///
    /// assert_eq!(PgNumeric::parse(" -1_000.50 ").unwrap().to_string(), "-1000.50");
    /// assert_eq!(PgNumeric::parse("0x1F").unwrap().to_string(), "31");
    /// assert_eq!(PgNumeric::parse("1e-3").unwrap().to_string(), "0.001");
    /// assert!(PgNumeric::parse("-inf").unwrap().is_infinite());
    /// assert_eq!(PgNumeric::parse("1e131072"), Err(NumericError::FormatOverflow));
    /// ```
    pub fn parse(text: &str) -> Result<Self, NumericError> {
        let syntax = NumericError::InvalidInputSyntax;
        let bytes = text.as_bytes();
        let start = bytes.iter().take_while(|&&b| is_pg_space(b)).count();
        let signed = &bytes[start..];
        let (negative, unsigned) = match signed.split_first() {
            Some((b'-', rest)) => (true, rest),
            Some((b'+', rest)) => (false, rest),
            _ => (false, signed),
        };
        let (value, rest) = match unsigned.first() {
            Some(b'0'..=b'9' | b'.') => {
                let (base, digits) = match unsigned {
                    [b'0', b'x' | b'X', digits @ ..] => (16, digits),
                    [b'0', b'o' | b'O', digits @ ..] => (8, digits),
                    [b'0', b'b' | b'B', digits @ ..] => (2, digits),
                    _ => (10, unsigned),
                };
                let (mut finite, rest) = if base == 10 {
                    parse_decimal(digits)?
                } else {
                    parse_non_decimal(digits, base)?
                };
                finite.negative = negative && !finite.digits.is_empty();
                (Repr::Finite(finite), rest)
            }
            _ => {
                if starts_with_ignore_case(signed, b"NaN") {
                    (Repr::NaN, &signed[3..])
                } else if starts_with_ignore_case(unsigned, b"Infinity") {
                    (Repr::Infinity { negative }, &unsigned[8..])
                } else if starts_with_ignore_case(unsigned, b"inf") {
                    (Repr::Infinity { negative }, &unsigned[3..])
                } else {
                    return Err(syntax);
                }
            }
        };
        if !rest.iter().all(|&b| is_pg_space(b)) {
            return Err(syntax);
        }
        if let Repr::Finite(finite) = &value {
            finite.check_storable()?;
        }
        Ok(Self(value))
    }

    /// Whether the value is `NaN`.
    #[must_use]
    pub const fn is_nan(&self) -> bool {
        matches!(self.0, Repr::NaN)
    }

    /// Whether the value is `Infinity` or `-Infinity`.
    #[must_use]
    pub const fn is_infinite(&self) -> bool {
        matches!(self.0, Repr::Infinity { .. })
    }

    /// Whether the value is zero, at any display scale.
    #[must_use]
    pub fn is_zero(&self) -> bool {
        matches!(&self.0, Repr::Finite(finite) if finite.digits.is_empty())
    }

    /// Whether the value is below zero, `-Infinity` included.
    #[must_use]
    pub const fn is_negative(&self) -> bool {
        matches!(
            self.0,
            Repr::Infinity { negative: true } | Repr::Finite(Finite { negative: true, .. })
        )
    }

    /// Whether the value is a finite whole number, at any display scale.
    ///
    /// ```
    /// use sql_values::PgNumeric;
    ///
    /// assert!(PgNumeric::parse("2.00").unwrap().is_integral());
    /// assert!(!PgNumeric::parse("2.5").unwrap().is_integral());
    /// assert!(!PgNumeric::INFINITY.is_integral());
    /// ```
    #[must_use]
    pub fn is_integral(&self) -> bool {
        matches!(&self.0, Repr::Finite(finite) if finite.exponent() >= 0)
    }

    /// The `numeric()` length coercion to `typmod`, rounding half away from zero.
    pub(crate) fn apply_typmod(self, typmod: NumericTypmod) -> Result<Self, CastError> {
        match self.0 {
            Repr::NaN => Ok(self),
            Repr::Infinity { .. } => Err(CastError::NumericFieldOverflow),
            Repr::Finite(mut finite) => {
                let scale = i64::from(typmod.scale);
                finite.round(scale);
                let integer_digits = i64::from(typmod.precision) - scale;
                if !finite.digits.is_empty() && finite.weight + 1 > integer_digits {
                    return Err(CastError::NumericFieldOverflow);
                }
                Ok(Self(Repr::Finite(finite)))
            }
        }
    }

    /// `numericvar_to_int64`, the value rounded half away from zero, or `None` for a value
    /// beyond `i64` or not finite.
    pub(crate) fn into_i64_rounded(self) -> Option<i64> {
        let Repr::Finite(mut rounded) = self.0 else {
            return None;
        };
        rounded.round(0);
        if rounded.weight >= 19 {
            return None;
        }
        let magnitude = (0..=rounded.weight).rev().fold(0i128, |magnitude, power| {
            magnitude * 10 + i128::from(rounded.digit_at(power))
        });
        i64::try_from(if rounded.negative {
            -magnitude
        } else {
            magnitude
        })
        .ok()
    }

    /// The order class of the value, rising from `-Infinity` through negative, zero, positive and
    /// `Infinity` to `NaN`.
    fn class(&self) -> i8 {
        match &self.0 {
            Repr::Infinity { negative: true } => -2,
            Repr::Finite(finite) if finite.digits.is_empty() => 0,
            Repr::Finite(finite) if finite.negative => -1,
            Repr::Finite(_) => 1,
            Repr::Infinity { negative: false } => 2,
            Repr::NaN => 3,
        }
    }

    /// What `cmp_numerics` orders the value by.
    fn key(&self) -> Key<'_> {
        match &self.0 {
            Repr::Finite(finite) => Key {
                class: self.class(),
                weight: finite.weight,
                digits: &finite.digits,
            },
            _ => Key {
                class: self.class(),
                weight: 0,
                digits: &[],
            },
        }
    }

    /// The order of the value against the `numeric` of an integer, without making it.
    pub(crate) fn cmp_integer(&self, value: i64) -> Ordering {
        self.key().order(&IntegerDigits::new(value).key())
    }

    /// Feeds `state` what the `numeric` of an integer feeds it, without making it.
    pub(crate) fn hash_integer<H: Hasher>(value: i64, state: &mut H) {
        IntegerDigits::new(value).key().hash(state);
    }
}

/// What `cmp_numerics` orders a value by, its order class and, for a nonzero finite value, the
/// weight and digits of its magnitude.
#[derive(Hash)]
struct Key<'a> {
    class: i8,
    weight: i64,
    digits: &'a [u8],
}

impl Key<'_> {
    /// The class first, then the magnitude, reversed for a negative value.
    fn order(&self, other: &Key<'_>) -> Ordering {
        self.class.cmp(&other.class).then_with(|| {
            let magnitude = self
                .weight
                .cmp(&other.weight)
                .then_with(|| self.digits.cmp(other.digits));
            if self.class < 0 {
                magnitude.reverse()
            } else {
                magnitude
            }
        })
    }
}

/// The decimal digits of an `i64` as `int64_to_numeric` makes them, normalised as [`Finite`]
/// holds them, without allocating.
struct IntegerDigits {
    negative: bool,
    /// The digits, the first `len` of them significant.
    digits: [u8; 19],
    len: usize,
    weight: i64,
}

impl IntegerDigits {
    fn new(value: i64) -> Self {
        let mut digits = [0; 19];
        let mut count = 0;
        let mut rest = value.unsigned_abs();
        while rest > 0 {
            digits[count] = u8::try_from(rest % 10).expect("a decimal digit fits u8");
            rest /= 10;
            count += 1;
        }
        digits[..count].reverse();
        let len = digits[..count]
            .iter()
            .rposition(|&digit| digit != 0)
            .map_or(0, |last| last + 1);
        let weight = if len == 0 {
            0
        } else {
            i64::try_from(count).expect("an i64 has 19 digits") - 1
        };
        Self {
            negative: value < 0,
            digits,
            len,
            weight,
        }
    }

    fn digits(&self) -> &[u8] {
        &self.digits[..self.len]
    }

    fn key(&self) -> Key<'_> {
        let class = match (self.len, self.negative) {
            (0, _) => 0,
            (_, true) => -1,
            (_, false) => 1,
        };
        Key {
            class,
            weight: self.weight,
            digits: self.digits(),
        }
    }
}

impl Finite {
    /// Drop leading and trailing zero digits, giving zero its unsigned form.
    fn normalise(&mut self) {
        let leading = self.digits.iter().take_while(|&&d| d == 0).count();
        self.digits.drain(..leading);
        self.weight -= i64::try_from(leading).expect("a digit count fits i64");
        let kept = self
            .digits
            .iter()
            .rposition(|&d| d != 0)
            .map_or(0, |last| last + 1);
        self.digits.truncate(kept);
        if self.digits.is_empty() {
            self.negative = false;
            self.weight = 0;
        }
    }

    /// The digit at `power` of ten.
    fn digit_at(&self, power: i64) -> u8 {
        usize::try_from(self.weight - power)
            .ok()
            .and_then(|index| self.digits.get(index).copied())
            .unwrap_or(0)
    }

    /// `round_var` to `scale` digits after the point, then `apply_typmod`'s non-negative display
    /// scale.
    fn round(&mut self, scale: i64) {
        self.dscale = scale.max(0);
        let kept = self.weight + scale + 1;
        let Ok(kept) = usize::try_from(kept) else {
            *self = Self::zero(self.dscale);
            return;
        };
        if kept >= self.digits.len() {
            return;
        }
        let round_up = self.digits[kept] >= 5;
        self.digits.truncate(kept);
        if round_up {
            let carried = self.digits.iter_mut().rev().try_for_each(|digit| {
                if *digit == 9 {
                    *digit = 0;
                    Ok(())
                } else {
                    *digit += 1;
                    Err(())
                }
            });
            if carried.is_ok() {
                self.digits.insert(0, 1);
                self.weight += 1;
            }
        }
        self.normalise();
    }

    const fn zero(dscale: i64) -> Self {
        Self {
            negative: false,
            digits: Vec::new(),
            weight: 0,
            dscale,
        }
    }

    /// `int64_to_numeric`.
    fn from_i64(value: i64) -> Self {
        let integer = IntegerDigits::new(value);
        Self {
            negative: integer.negative,
            digits: integer.digits().to_vec(),
            weight: integer.weight,
            dscale: 0,
        }
    }

    /// The power of ten of the last digit, `1` for zero.
    fn exponent(&self) -> i64 {
        self.weight + 1 - i64::try_from(self.digits.len()).expect("a digit count fits i64")
    }

    /// The digits as an integer followed by `zeros` zeros, the value's magnitude over
    /// `10^(exponent - zeros)`.
    fn mantissa(&self, zeros: i64) -> Magnitude {
        let zeros = usize::try_from(zeros).expect("a shift toward a lower exponent");
        Magnitude::from_digits(&self.digits, zeros)
    }

    /// The value `±mantissa × 10^exponent` printed with `dscale` digits after the point.
    fn from_mantissa(negative: bool, mantissa: &Magnitude, exponent: i64, dscale: i64) -> Self {
        let digits = mantissa.decimal_digits();
        let weight = exponent + i64::try_from(digits.len()).expect("a digit count fits i64") - 1;
        let mut finite = Self {
            negative,
            digits,
            weight,
            dscale,
        };
        finite.normalise();
        finite
    }

    /// `trunc_var` toward zero at `scale` digits after the point, with a non-negative display
    /// scale.
    fn truncate(&mut self, scale: i64) {
        self.dscale = scale.max(0);
        let kept = usize::try_from(self.weight + scale + 1).unwrap_or(0);
        if kept < self.digits.len() {
            self.digits.truncate(kept);
            self.normalise();
        }
    }

    /// `make_result`'s check that the weight and display scale fit their stored fields.
    fn check_storable(&self) -> Result<(), NumericError> {
        let weight_fits = self.digits.is_empty() || self.weight <= MAX_WEIGHT;
        if weight_fits && self.dscale <= MAX_DSCALE {
            Ok(())
        } else {
            Err(NumericError::FormatOverflow)
        }
    }
}

/// `set_var_from_str` from the first digit or point, returning the value and the unread rest.
fn parse_decimal(bytes: &[u8]) -> Result<(Finite, &[u8]), NumericError> {
    let syntax = NumericError::InvalidInputSyntax;
    let mut digits = Vec::new();
    let mut integer_digits: i64 = 0;
    let mut dscale: i64 = 0;
    let mut have_point = false;
    let mut at = 0;
    if bytes.first() == Some(&b'.') {
        have_point = true;
        at = 1;
    }
    if !bytes.get(at).is_some_and(u8::is_ascii_digit) {
        return Err(syntax);
    }
    while let Some(&byte) = bytes.get(at) {
        match byte {
            b'0'..=b'9' => {
                digits.push(byte - b'0');
                if have_point {
                    dscale += 1;
                } else {
                    integer_digits += 1;
                }
                at += 1;
            }
            b'.' if have_point => return Err(syntax),
            b'.' => {
                have_point = true;
                at += 1;
                if bytes.get(at) == Some(&b'_') {
                    return Err(syntax);
                }
            }
            b'_' => {
                at += 1;
                if !bytes.get(at).is_some_and(u8::is_ascii_digit) {
                    return Err(syntax);
                }
            }
            _ => break,
        }
    }
    let mut weight = integer_digits - 1;
    if matches!(bytes.get(at), Some(b'e' | b'E')) {
        at += 1;
        let negative = match bytes.get(at) {
            Some(b'+') => {
                at += 1;
                false
            }
            Some(b'-') => {
                at += 1;
                true
            }
            _ => false,
        };
        if !bytes.get(at).is_some_and(u8::is_ascii_digit) {
            return Err(syntax);
        }
        let mut exponent: i64 = 0;
        while let Some(&byte) = bytes.get(at) {
            match byte {
                b'0'..=b'9' => {
                    exponent = exponent * 10 + i64::from(byte - b'0');
                    if exponent > MAX_EXPONENT {
                        return Err(NumericError::FormatOverflow);
                    }
                    at += 1;
                }
                b'_' => {
                    at += 1;
                    if !bytes.get(at).is_some_and(u8::is_ascii_digit) {
                        return Err(syntax);
                    }
                }
                _ => break,
            }
        }
        let exponent = if negative { -exponent } else { exponent };
        weight += exponent;
        dscale = (dscale - exponent).max(0);
    }
    let mut finite = Finite {
        negative: false,
        digits,
        weight,
        dscale,
    };
    finite.normalise();
    Ok((finite, &bytes[at..]))
}

/// `set_var_from_non_decimal_integer_str` after the base prefix, checking the weight after each
/// group of digits that fills an `int64` as PostgreSQL does.
fn parse_non_decimal(bytes: &[u8], base: u32) -> Result<(Finite, &[u8]), NumericError> {
    let syntax = NumericError::InvalidInputSyntax;
    // `mul > PG_INT64_MAX / base` starts a new group after this many digits.
    let group_len = match base {
        16 => 15,
        8 => 20,
        _ => 62,
    };
    let mut limbs = Magnitude::default();
    let mut group: u64 = 0;
    let mut group_digits: u32 = 0;
    let mut at = 0;
    while let Some(&byte) = bytes.get(at) {
        if let Some(digit) = char::from(byte).to_digit(base) {
            if group_digits == group_len {
                limbs.push_group(group, base, group_digits);
                if limbs.decimal_weight() > MAX_WEIGHT {
                    return Err(NumericError::FormatOverflow);
                }
                group = 0;
                group_digits = 0;
            }
            group = group * u64::from(base) + u64::from(digit);
            group_digits += 1;
            at += 1;
        } else if byte == b'_' {
            at += 1;
            let next_is_digit = bytes
                .get(at)
                .is_some_and(|&next| char::from(next).is_digit(base));
            if !next_is_digit {
                return Err(syntax);
            }
        } else {
            break;
        }
    }
    if at == 0 {
        return Err(syntax);
    }
    limbs.push_group(group, base, group_digits);
    if limbs.decimal_weight() > MAX_WEIGHT {
        return Err(NumericError::FormatOverflow);
    }
    Ok((Finite::from_mantissa(false, &limbs, 0, 0), &bytes[at..]))
}

/// `pg_strncasecmp(bytes, word, word.len()) == 0`.
fn starts_with_ignore_case(bytes: &[u8], word: &[u8]) -> bool {
    bytes
        .get(..word.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(word))
}

/// `numeric_out`.
impl fmt::Display for PgNumeric {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let finite = match &self.0 {
            Repr::NaN => return f.write_str("NaN"),
            Repr::Infinity { negative: false } => return f.write_str("Infinity"),
            Repr::Infinity { negative: true } => return f.write_str("-Infinity"),
            Repr::Finite(finite) => finite,
        };
        if finite.negative {
            f.write_str("-")?;
        }
        let digit = |power| char::from(b'0' + finite.digit_at(power));
        if finite.weight < 0 || finite.digits.is_empty() {
            f.write_str("0")?;
        } else {
            for power in (0..=finite.weight).rev() {
                fmt::Write::write_char(f, digit(power))?;
            }
        }
        if finite.dscale > 0 {
            f.write_str(".")?;
            for power in 1..=finite.dscale {
                fmt::Write::write_char(f, digit(-power))?;
            }
        }
        Ok(())
    }
}

impl PartialEq for PgNumeric {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for PgNumeric {}

impl PartialOrd for PgNumeric {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// `cmp_numerics`, ordering `-Infinity` below finite values by value, then `Infinity`, then `NaN`.
impl Ord for PgNumeric {
    fn cmp(&self, other: &Self) -> Ordering {
        self.key().order(&other.key())
    }
}

impl Hash for PgNumeric {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.key().hash(state);
    }
}

/// `int2_numeric`.
impl From<i16> for PgNumeric {
    fn from(value: i16) -> Self {
        Self::from(i64::from(value))
    }
}

/// `int4_numeric`.
impl From<i32> for PgNumeric {
    fn from(value: i32) -> Self {
        Self::from(i64::from(value))
    }
}

/// `int8_numeric`.
impl From<i64> for PgNumeric {
    fn from(value: i64) -> Self {
        Self(Repr::Finite(Finite::from_i64(value)))
    }
}
