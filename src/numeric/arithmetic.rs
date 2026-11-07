//! PostgreSQL 18's `numeric` operators and rounding functions, `numeric_add`, `numeric_sub`,
//! `numeric_mul`, `numeric_div`, `numeric_mod`, `numeric_uminus`, `numeric_abs`,
//! `numeric_round`, `numeric_trunc`, `numeric_floor` and `numeric_ceil`.

use core::{cmp::Ordering, ops::Neg};

use super::{Finite, MAX_DSCALE, MAX_WEIGHT, NumericError, PgNumeric, Repr};

/// `NUMERIC_MIN_SIG_DIGITS`, the significant digits a quotient keeps at least.
const MIN_SIG_DIGITS: i64 = 16;
/// `NUMERIC_MAX_DISPLAY_SCALE`, the most digits after the point a quotient takes.
const MAX_DISPLAY_SCALE: i64 = 1000;
/// `DEC_DIGITS`, the decimal digits in one of PostgreSQL's base-10000 digits.
const DEC_DIGITS: i64 = 4;

impl PgNumeric {
    /// `numeric + numeric`, PostgreSQL's `numeric_add`, exact at the larger display scale.
    /// `NaN` with anything, and opposite infinities, give `NaN`.
    ///
    /// # Errors
    ///
    /// [`NumericError::FormatOverflow`] for a sum beyond what PostgreSQL stores.
    ///
    /// ```
    /// use sql_values::PgNumeric;
    ///
    /// let numeric = |text| PgNumeric::parse(text).unwrap();
    /// assert_eq!(numeric("1.50").checked_add(&numeric("2.250")).unwrap().to_string(), "3.750");
    /// assert!(PgNumeric::INFINITY.checked_add(&PgNumeric::NEG_INFINITY).unwrap().is_nan());
    /// ```
    pub fn checked_add(&self, other: &Self) -> Result<Self, NumericError> {
        match (&self.0, &other.0) {
            (Repr::NaN, _) | (_, Repr::NaN) => Ok(Self::NAN),
            (Repr::Infinity { negative: left }, Repr::Infinity { negative: right })
                if left != right =>
            {
                Ok(Self::NAN)
            }
            (Repr::Infinity { .. }, _) => Ok(self.clone()),
            (_, Repr::Infinity { .. }) => Ok(other.clone()),
            (Repr::Finite(left), Repr::Finite(right)) => finite(left.add(right, false)),
        }
    }

    /// `numeric - numeric`, PostgreSQL's `numeric_sub`, exact at the larger display scale.
    /// `NaN` with anything, and like infinities, give `NaN`.
    ///
    /// # Errors
    ///
    /// [`NumericError::FormatOverflow`] for a difference beyond what PostgreSQL stores.
    pub fn checked_sub(&self, other: &Self) -> Result<Self, NumericError> {
        match (&self.0, &other.0) {
            (Repr::NaN, _) | (_, Repr::NaN) => Ok(Self::NAN),
            (Repr::Infinity { negative: left }, Repr::Infinity { negative: right })
                if left == right =>
            {
                Ok(Self::NAN)
            }
            (Repr::Infinity { .. }, _) => Ok(self.clone()),
            (_, &Repr::Infinity { negative }) => Ok(Self(Repr::Infinity {
                negative: !negative,
            })),
            (Repr::Finite(left), Repr::Finite(right)) => finite(left.add(right, true)),
        }
    }

    /// `numeric * numeric`, PostgreSQL's `numeric_mul`, exact at the sum of the display scales,
    /// rounded to 16383 digits after the point beyond them. `NaN` with anything, and an infinity
    /// by zero, give `NaN`.
    ///
    /// # Errors
    ///
    /// [`NumericError::FormatOverflow`] for a product beyond what PostgreSQL stores.
    ///
    /// ```
    /// use sql_values::{NumericError, PgNumeric};
    ///
    /// let numeric = |text| PgNumeric::parse(text).unwrap();
    /// assert_eq!(numeric("1.5").checked_mul(&numeric("2.25")).unwrap().to_string(), "3.375");
    /// assert!(PgNumeric::INFINITY.checked_mul(&numeric("0")).unwrap().is_nan());
    /// assert_eq!(
    ///     numeric("1e131071").checked_mul(&numeric("10")),
    ///     Err(NumericError::FormatOverflow),
    /// );
    /// ```
    pub fn checked_mul(&self, other: &Self) -> Result<Self, NumericError> {
        match (&self.0, &other.0) {
            (Repr::NaN, _) | (_, Repr::NaN) => Ok(Self::NAN),
            (Repr::Finite(left), Repr::Finite(right)) => finite(left.mul(right)),
            _ if self.is_zero() || other.is_zero() => Ok(Self::NAN),
            _ => Ok(Self(Repr::Infinity {
                negative: self.is_negative() != other.is_negative(),
            })),
        }
    }

    /// `numeric / numeric`, PostgreSQL's `numeric_div`, rounded half away from zero to the scale
    /// `select_div_scale` picks, at least 16 significant digits and at least either display
    /// scale, up to 1000 digits after the point. `NaN` with anything, and an infinity by an
    /// infinity, give `NaN`, and a finite value by an infinity gives zero.
    ///
    /// # Errors
    ///
    /// [`NumericError::DivisionByZero`] for a divisor of zero, and
    /// [`NumericError::FormatOverflow`] for a quotient beyond what PostgreSQL stores.
    ///
    /// ```
    /// use sql_values::{NumericError, PgNumeric};
    ///
    /// let numeric = |text| PgNumeric::parse(text).unwrap();
    /// let third = numeric("10").checked_div(&numeric("3")).unwrap();
    /// assert_eq!(third.to_string(), "3.3333333333333333");
    /// assert_eq!(
    ///     PgNumeric::INFINITY.checked_div(&numeric("0")),
    ///     Err(NumericError::DivisionByZero),
    /// );
    /// ```
    pub fn checked_div(&self, other: &Self) -> Result<Self, NumericError> {
        match (&self.0, &other.0) {
            (Repr::NaN, _) | (_, Repr::NaN) | (Repr::Infinity { .. }, Repr::Infinity { .. }) => {
                Ok(Self::NAN)
            }
            (Repr::Finite(left), Repr::Finite(right)) => finite(left.div(right)?),
            (Repr::Finite(_), Repr::Infinity { .. }) => Ok(Self(Repr::Finite(Finite::zero(0)))),
            (Repr::Infinity { negative }, Repr::Finite(right)) => {
                if right.digits.is_empty() {
                    Err(NumericError::DivisionByZero)
                } else {
                    Ok(Self(Repr::Infinity {
                        negative: *negative != right.negative,
                    }))
                }
            }
        }
    }

    /// `numeric % numeric`, PostgreSQL's `numeric_mod`, the remainder of the quotient truncated
    /// toward zero, with the dividend's sign, at the larger display scale. `NaN` with anything,
    /// and an infinity by anything but zero, give `NaN`, and a finite value by an infinity is
    /// itself.
    ///
    /// # Errors
    ///
    /// [`NumericError::DivisionByZero`] for a divisor of zero.
    ///
    /// ```
    /// use sql_values::PgNumeric;
    ///
    /// let numeric = |text| PgNumeric::parse(text).unwrap();
    /// assert_eq!(numeric("-7.5").checked_rem(&numeric("2")).unwrap().to_string(), "-1.5");
    /// ```
    pub fn checked_rem(&self, other: &Self) -> Result<Self, NumericError> {
        match (&self.0, &other.0) {
            (Repr::NaN, _) | (_, Repr::NaN) => Ok(Self::NAN),
            (Repr::Infinity { .. }, _) if other.is_zero() => Err(NumericError::DivisionByZero),
            (Repr::Infinity { .. }, _) => Ok(Self::NAN),
            (Repr::Finite(_), Repr::Infinity { .. }) => Ok(self.clone()),
            (Repr::Finite(left), Repr::Finite(right)) => finite(left.rem(right)?),
        }
    }

    /// `abs(numeric)`, PostgreSQL's `numeric_abs`, keeping the display scale.
    #[must_use]
    pub fn abs(mut self) -> Self {
        match &mut self.0 {
            Repr::NaN => {}
            Repr::Infinity { negative } => *negative = false,
            Repr::Finite(finite) => finite.negative = false,
        }
        self
    }

    /// `round(numeric, scale)`, PostgreSQL's `numeric_round`, half away from zero to `scale`
    /// digits after the point, before it for a negative scale, with a display scale of `scale`
    /// or zero. `round(numeric)` is a scale of `0`. `NaN` and the infinities are left as they
    /// are.
    ///
    /// # Errors
    ///
    /// [`NumericError::FormatOverflow`] where rounding up carries past what PostgreSQL stores.
    ///
    /// ```
    /// use sql_values::PgNumeric;
    ///
    /// let numeric = |text| PgNumeric::parse(text).unwrap();
    /// assert_eq!(numeric("-2.5").round(0).unwrap().to_string(), "-3");
    /// assert_eq!(numeric("1234.5678").round(-2).unwrap().to_string(), "1200");
    /// assert_eq!(numeric("1.5").round(3).unwrap().to_string(), "1.500");
    /// ```
    pub fn round(self, scale: i32) -> Result<Self, NumericError> {
        let Repr::Finite(mut rounded) = self.0 else {
            return Ok(self);
        };
        // An extra digit before the point, in case the leading digit rounds up.
        rounded.round(i64::from(scale).clamp(-(MAX_WEIGHT + 2), MAX_DSCALE));
        finite(rounded)
    }

    /// `trunc(numeric, scale)`, PostgreSQL's `numeric_trunc`, toward zero at `scale` digits after
    /// the point, before it for a negative scale, with a display scale of `scale` or zero.
    /// `trunc(numeric)` is a scale of `0`. `NaN` and the infinities are left as they are.
    ///
    /// ```
    /// use sql_values::PgNumeric;
    ///
    /// assert_eq!(PgNumeric::parse("-1.99").unwrap().trunc(1).to_string(), "-1.9");
    /// ```
    #[must_use]
    pub fn trunc(self, scale: i32) -> Self {
        let Repr::Finite(mut truncated) = self.0 else {
            return self;
        };
        truncated.truncate(i64::from(scale).clamp(-(MAX_WEIGHT + 1), MAX_DSCALE));
        Self(Repr::Finite(truncated))
    }

    /// `floor(numeric)`, PostgreSQL's `numeric_floor`, the greatest whole number not above the
    /// value. `NaN` and the infinities are left as they are.
    ///
    /// # Errors
    ///
    /// [`NumericError::FormatOverflow`] where the step down carries past what PostgreSQL stores.
    ///
    /// ```
    /// use sql_values::PgNumeric;
    ///
    /// assert_eq!(PgNumeric::parse("-0.5").unwrap().floor().unwrap().to_string(), "-1");
    /// ```
    pub fn floor(self) -> Result<Self, NumericError> {
        self.whole(true)
    }

    /// `ceil(numeric)`, PostgreSQL's `numeric_ceil`, the least whole number not below the value.
    /// `NaN` and the infinities are left as they are.
    ///
    /// # Errors
    ///
    /// [`NumericError::FormatOverflow`] where the step up carries past what PostgreSQL stores.
    ///
    /// ```
    /// use sql_values::PgNumeric;
    ///
    /// assert_eq!(PgNumeric::parse("-0.5").unwrap().ceil().unwrap().to_string(), "0");
    /// ```
    pub fn ceil(self) -> Result<Self, NumericError> {
        self.whole(false)
    }

    /// `floor_var` when `down`, else `ceil_var`, the value truncated and then stepped by one away
    /// from zero when that dropped a fraction on the side it rounds toward.
    fn whole(self, down: bool) -> Result<Self, NumericError> {
        let Repr::Finite(value) = self.0 else {
            return Ok(self);
        };
        let had_fraction = !value.digits.is_empty() && value.exponent() < 0;
        let negative = value.negative;
        let mut whole = value;
        whole.truncate(0);
        if had_fraction && negative == down {
            let step = Finite::from_i64(if down { -1 } else { 1 });
            whole = whole.add(&step, false);
        }
        finite(whole)
    }
}

/// `numeric_uminus`, flipping the sign of every value but zero and `NaN`, keeping the display
/// scale.
impl Neg for PgNumeric {
    type Output = Self;

    fn neg(mut self) -> Self {
        match &mut self.0 {
            Repr::NaN => {}
            Repr::Infinity { negative } => *negative = !*negative,
            Repr::Finite(finite) => finite.negative = !finite.negative && !finite.digits.is_empty(),
        }
        self
    }
}

impl Finite {
    /// `add_var`, or `sub_var` when `subtract`, exact at the larger display scale.
    fn add(&self, other: &Self, subtract: bool) -> Self {
        let other_negative = other.negative != subtract;
        let exponent = self.exponent().min(other.exponent());
        let left = self.mantissa(self.exponent() - exponent);
        let right = other.mantissa(other.exponent() - exponent);
        let dscale = self.dscale.max(other.dscale);
        let (negative, magnitude) = if self.negative == other_negative {
            (self.negative, left.add(&right))
        } else {
            match left.cmp(&right) {
                Ordering::Less => (other_negative, right.sub(&left)),
                _ => (self.negative, left.sub(&right)),
            }
        };
        Self::from_mantissa(negative, &magnitude, exponent, dscale)
    }

    /// `numeric_mul`'s exact `mul_var`, rounded to `NUMERIC_DSCALE_MAX` beyond it.
    fn mul(&self, other: &Self) -> Self {
        let product = self.mantissa(0).mul(&other.mantissa(0));
        let mut result = Self::from_mantissa(
            self.negative != other.negative,
            &product,
            self.exponent() + other.exponent(),
            self.dscale + other.dscale,
        );
        if result.dscale > MAX_DSCALE {
            result.round(MAX_DSCALE);
        }
        result
    }

    /// `numeric_div`'s exact `div_var` rounded at the scale `select_div_scale` picks.
    fn div(&self, other: &Self) -> Result<Self, NumericError> {
        if other.digits.is_empty() {
            return Err(NumericError::DivisionByZero);
        }
        let scale = self.select_div_scale(other);
        // The exact quotient truncated one digit further rounds half away from zero at `scale`.
        let mut quotient = self.truncated_quotient(other, scale + 1);
        quotient.round(scale);
        Ok(quotient)
    }

    /// `mod_var`, `self - other * trunc(self / other)`, at the larger display scale.
    fn rem(&self, other: &Self) -> Result<Self, NumericError> {
        if other.digits.is_empty() {
            return Err(NumericError::DivisionByZero);
        }
        let exponent = self.exponent().min(other.exponent());
        let dividend = self.mantissa(self.exponent() - exponent);
        let divisor = other.mantissa(other.exponent() - exponent);
        let (_, remainder) = dividend.div_rem(&divisor);
        let dscale = self.dscale.max(other.dscale);
        Ok(Self::from_mantissa(
            self.negative,
            &remainder,
            exponent,
            dscale,
        ))
    }

    /// The exact quotient truncated toward zero at `scale` digits after the point.
    fn truncated_quotient(&self, other: &Self, scale: i64) -> Self {
        let shift = self.exponent() - other.exponent() + scale;
        let (dividend, divisor) = if shift >= 0 {
            (self.mantissa(shift), other.mantissa(0))
        } else {
            (self.mantissa(0), other.mantissa(-shift))
        };
        let (quotient, _) = dividend.div_rem(&divisor);
        Self::from_mantissa(
            self.negative != other.negative,
            &quotient,
            -scale,
            scale.max(0),
        )
    }

    /// `select_div_scale`, enough digits after the point for 16 significant digits of the
    /// quotient as PostgreSQL estimates its weight in base-10000 digits, at least either display
    /// scale, at most 1000.
    fn select_div_scale(&self, other: &Self) -> i64 {
        let (left_weight, left_first) = self.leading_nbase_digit();
        let (right_weight, right_first) = other.leading_nbase_digit();
        let mut quotient_weight = left_weight - right_weight;
        if left_first <= right_first {
            quotient_weight -= 1;
        }
        (MIN_SIG_DIGITS - quotient_weight * DEC_DIGITS)
            .max(self.dscale)
            .max(other.dscale)
            .clamp(0, MAX_DISPLAY_SCALE)
    }

    /// The weight and value of the leading base-10000 digit as PostgreSQL stores the value,
    /// `(0, 0)` for zero.
    fn leading_nbase_digit(&self) -> (i64, u16) {
        if self.digits.is_empty() {
            return (0, 0);
        }
        let weight = self.weight.div_euclid(DEC_DIGITS);
        let first = (0..DEC_DIGITS).rev().fold(0, |value, offset| {
            value * 10 + u16::from(self.digit_at(weight * DEC_DIGITS + offset))
        });
        (weight, first)
    }
}

/// `make_result`, the finite value once checked to fit what PostgreSQL stores.
fn finite(value: Finite) -> Result<PgNumeric, NumericError> {
    value.check_storable()?;
    Ok(PgNumeric(Repr::Finite(value)))
}
