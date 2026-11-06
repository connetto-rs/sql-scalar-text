#![no_std]
#![doc = include_str!("../README.md")]

extern crate alloc;

mod cast;
mod datetime;
mod float;
mod interval;
mod numeric;
mod zone;

pub use cast::{CastError, CastTarget, CastValue, CharLength, Literal, PgType, cast_literal};
pub use datetime::{
    DateOrder, DateTimeError, InputContext, PgDate, PgTime, PgTimeTz, PgTimestamp, PgTimestampTz,
    parse_date, parse_time, parse_timestamp, parse_timestamp_tz,
};
pub use interval::{
    IntervalError, IntervalFields, IntervalSum, IntervalTypmod, PgInterval, ShiftError,
    parse_interval, parse_interval_typmod,
};
pub use numeric::{NumericTypmod, PgNumeric};
pub use zone::{PgTimeZone, TimeZoneAbbreviations, TimeZoneAbbreviationsError, TimeZoneError};

use alloc::vec::Vec;

/// Parse `t`, `f`, `1`, or `0` as a boolean.
///
/// ```
/// assert_eq!(sql_values::parse_bool("0"), Some(false));
/// ```
pub fn parse_bool(text: &str) -> Option<bool> {
    match text {
        "t" | "1" => Some(true),
        "f" | "0" => Some(false),
        _ => None,
    }
}

/// Parse PostgreSQL `\x`-prefixed bytea hex with no intermediate allocation.
///
/// ```
/// assert_eq!(sql_values::parse_pg_bytea_hex(r"\x4869"), Some(vec![0x48, 0x69]));
/// ```
pub fn parse_pg_bytea_hex(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    if bytes.len() < 2 || bytes[0] != b'\\' || bytes[1] != b'x' {
        return None;
    }

    let hex_part = &bytes[2..];
    if !hex_part.len().is_multiple_of(2) {
        return None;
    }

    let mut result = Vec::with_capacity(hex_part.len() / 2);
    for i in (0..hex_part.len()).step_by(2) {
        let high = hex_digit(hex_part[i])?;
        let low = hex_digit(hex_part[i + 1])?;
        result.push((high << 4) | low);
    }
    Some(result)
}

/// Parse integer text with an optional leading sign.
///
/// ```
/// assert_eq!(sql_values::parse_i64("-42"), Some(-42));
/// ```
pub fn parse_i64(text: &str) -> Option<i64> {
    text.parse::<i64>().ok()
}

/// Parse a 64-bit float, including PostgreSQL infinity and `NaN` spellings.
///
/// ```
/// assert_eq!(sql_values::parse_f64("Infinity"), Some(f64::INFINITY));
/// ```
pub fn parse_f64(text: &str) -> Option<f64> {
    match text {
        "Infinity" => Some(f64::INFINITY),
        "-Infinity" => Some(f64::NEG_INFINITY),
        "NaN" => Some(f64::NAN),
        _ => text.parse::<f64>().ok(),
    }
}

/// Parse an arbitrary-precision decimal.
///
/// ```
/// let d = sql_values::parse_decimal("123.450").unwrap();
/// assert_eq!(d.to_string(), "123.450");
/// ```
#[cfg(feature = "decimal")]
pub fn parse_decimal(text: &str) -> Option<bigdecimal::BigDecimal> {
    use core::str::FromStr;
    bigdecimal::BigDecimal::from_str(text).ok()
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
