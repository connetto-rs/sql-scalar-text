//! C's `double` conversions and rounding as PostgreSQL's `timestamp.c` and `datetime.c` use them,
//! with no `libm`.

/// `rint` in the default rounding mode, ties to even.
pub(super) fn round_ties_even(value: f64) -> f64 {
    const TWO_POW_52: f64 = 4_503_599_627_370_496.0;
    let magnitude = if value.is_sign_negative() {
        -value
    } else {
        value
    };
    if magnitude >= TWO_POW_52 {
        return value;
    }
    // At 2^52 no fraction bits are left, so the addition rounds ties to even.
    let rounded = (magnitude + TWO_POW_52) - TWO_POW_52;
    if value.is_sign_negative() {
        -rounded
    } else {
        rounded
    }
}

/// `TSROUND`, rounding to the microsecond.
pub(super) fn ts_round(value: f64) -> f64 {
    const TS_PREC_INV: f64 = 1_000_000.0;
    round_ties_even(value * TS_PREC_INV) / TS_PREC_INV
}

/// `FLOAT8_FITS_IN_INT32`.
pub(super) fn fits_in_i32(value: f64) -> bool {
    !value.is_nan() && value >= f64::from(i32::MIN) && value < -f64::from(i32::MIN)
}

/// `FLOAT8_FITS_IN_INT64`.
pub(super) fn fits_in_i64(value: f64) -> bool {
    !value.is_nan() && value >= int_to_f64(i64::MIN) && value < -int_to_f64(i64::MIN)
}

/// C's `(int)` conversion of a double the caller has bounded inside `int`.
pub(super) fn truncate_to_i32(value: f64) -> i32 {
    debug_assert!(fits_in_i32(value), "{value} does not fit an int");
    // Intended loss, as C's `(int)` cast truncates towards zero.
    value as i32
}

/// C's `(int64)` conversion of a double the caller has bounded inside `int64`.
pub(super) fn truncate_to_i64(value: f64) -> i64 {
    debug_assert!(fits_in_i64(value), "{value} does not fit an int64");
    // Intended loss, as C's `(int64)` cast truncates towards zero.
    value as i64
}

/// C's conversion of an integer operand to `double`.
pub(super) fn int_to_f64(value: i64) -> f64 {
    // Intended loss, as C rounds an `int64` beyond 2^53 to the nearest double.
    value as f64
}
