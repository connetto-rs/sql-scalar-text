#![no_std]
#![doc = include_str!("../README.md")]

extern crate alloc;

mod interval;

pub use interval::{
    IntervalFields, IntervalTypmod, PgInterval, parse_interval, parse_interval_typmod,
};

use alloc::vec::Vec;
use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};

/// Parse `YYYY-MM-DD HH:MM:SS[.f]` with a space or `T` separator, a year of four or more digits
/// and PostgreSQL's trailing ` BC`.
///
/// ```
/// let dt = sql_scalar_text::parse_timestamp("2024-01-02 03:04:05.125").unwrap();
/// assert_eq!(dt.format("%Y-%m-%d %H:%M:%S%.f").to_string(), "2024-01-02 03:04:05.125");
/// ```
pub fn parse_timestamp(text: &str) -> Option<NaiveDateTime> {
    let (body, bc) = split_era(text.as_bytes());
    match parse_datetime_prefix(body, bc)? {
        (dt, []) => Some(dt),
        _ => None,
    }
}

/// Parse a [`parse_timestamp`] layout followed by a `Z`, `±HH`, `±HHMM`, `±HH:MM` or `±HH:MM:SS`
/// offset, then PostgreSQL's optional ` BC`.
///
/// ```
/// let dt = sql_scalar_text::parse_timestamp_tz("2024-01-02T03:04:05+02:00").unwrap();
/// assert_eq!(dt.to_rfc3339(), "2024-01-02T01:04:05+00:00");
/// ```
pub fn parse_timestamp_tz(text: &str) -> Option<DateTime<Utc>> {
    let (body, bc) = split_era(text.as_bytes());
    let (local, offset) = parse_datetime_prefix(body, bc)?;
    let offset = parse_pg_offset(offset)?;
    Some(
        offset
            .from_local_datetime(&local)
            .single()?
            .with_timezone(&Utc),
    )
}

/// Parse a `YYYY-MM-DD` date with a year of four or more digits and PostgreSQL's trailing ` BC`.
///
/// ```
/// let d = sql_scalar_text::parse_date("2024-01-02").unwrap();
/// assert_eq!(d.to_string(), "2024-01-02");
/// ```
pub fn parse_date(text: &str) -> Option<NaiveDate> {
    let (body, bc) = split_era(text.as_bytes());
    match parse_date_prefix(body, bc)? {
        (date, []) => Some(date),
        _ => None,
    }
}

/// Parse `HH:MM:SS[.f]` with one to nine fraction digits.
///
/// ```
/// let t = sql_scalar_text::parse_time("03:04:05.125").unwrap();
/// assert_eq!(t.to_string(), "03:04:05.125");
/// ```
pub fn parse_time(text: &str) -> Option<NaiveTime> {
    match parse_time_prefix(text.as_bytes())? {
        (time, []) => Some(time),
        _ => None,
    }
}

/// Split off PostgreSQL's ` BC`, which trails the whole value.
fn split_era(bytes: &[u8]) -> (&[u8], bool) {
    match bytes.strip_suffix(b" BC") {
        Some(body) => (body, true),
        None => (bytes, false),
    }
}

/// Parse a leading `Y{4,}-MM-DD`, where year `Y` BC is chrono's year `1 - Y` and year `0000`, as
/// MySQL and SQLite print it, is chrono's year 0.
fn parse_date_prefix(bytes: &[u8], bc: bool) -> Option<(NaiveDate, &[u8])> {
    let year_len = bytes.iter().take_while(|b| b.is_ascii_digit()).count();
    if year_len < 4 || (year_len > 4 && bytes[0] == b'0') {
        return None;
    }
    let (year_digits, rest) = bytes.split_at(year_len);
    let year = year_digits.iter().try_fold(0i32, |year, &digit| {
        year.checked_mul(10)?.checked_add(i32::from(digit - b'0'))
    })?;
    let year = match (year, bc) {
        (0, true) => return None,
        (_, true) => 1 - year,
        (_, false) => year,
    };
    let Some((&[b'-', m1, m2, b'-', d1, d2], rest)) = rest.split_first_chunk() else {
        return None;
    };
    let month = parse_two_digits(&[m1, m2])?;
    let day = parse_two_digits(&[d1, d2])?;
    Some((
        NaiveDate::from_ymd_opt(year, month.into(), day.into())?,
        rest,
    ))
}

/// Parse a leading `HH:MM:SS[.f]` with one to nine fraction digits.
fn parse_time_prefix(bytes: &[u8]) -> Option<(NaiveTime, &[u8])> {
    let Some((&[h1, h2, b':', m1, m2, b':', s1, s2], rest)) = bytes.split_first_chunk() else {
        return None;
    };
    let (nanos, rest) = match rest.split_first() {
        Some((b'.', fraction)) => {
            let len = fraction.iter().take_while(|b| b.is_ascii_digit()).count();
            if !(1..=9).contains(&len) {
                return None;
            }
            let (digits, rest) = fraction.split_at(len);
            let value = digits
                .iter()
                .fold(0u32, |value, &digit| value * 10 + u32::from(digit - b'0'));
            ((len..9).fold(value, |nanos, _| nanos * 10), rest)
        }
        _ => (0, rest),
    };
    let time = NaiveTime::from_hms_nano_opt(
        parse_two_digits(&[h1, h2])?.into(),
        parse_two_digits(&[m1, m2])?.into(),
        parse_two_digits(&[s1, s2])?.into(),
        nanos,
    )?;
    Some((time, rest))
}

/// Parse a leading date and time joined by a space or `T`.
fn parse_datetime_prefix(bytes: &[u8], bc: bool) -> Option<(NaiveDateTime, &[u8])> {
    let (date, rest) = parse_date_prefix(bytes, bc)?;
    let (&(b' ' | b'T'), rest) = rest.split_first()? else {
        return None;
    };
    let (time, rest) = parse_time_prefix(rest)?;
    Some((date.and_time(time), rest))
}

/// Parse `t`, `f`, `1`, or `0` as a boolean.
///
/// ```
/// assert_eq!(sql_scalar_text::parse_bool("0"), Some(false));
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
/// assert_eq!(sql_scalar_text::parse_pg_bytea_hex(r"\x4869"), Some(vec![0x48, 0x69]));
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
/// assert_eq!(sql_scalar_text::parse_i64("-42"), Some(-42));
/// ```
pub fn parse_i64(text: &str) -> Option<i64> {
    text.parse::<i64>().ok()
}

/// Parse a 64-bit float, including PostgreSQL infinity and `NaN` spellings.
///
/// ```
/// assert_eq!(sql_scalar_text::parse_f64("Infinity"), Some(f64::INFINITY));
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
/// let d = sql_scalar_text::parse_decimal("123.450").unwrap();
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

fn parse_two_digits(bytes: &[u8]) -> Option<u8> {
    match *bytes {
        [tens @ b'0'..=b'9', ones @ b'0'..=b'9', ..] => Some((tens - b'0') * 10 + (ones - b'0')),
        _ => None,
    }
}

fn parse_pg_offset(bytes: &[u8]) -> Option<FixedOffset> {
    if bytes == b"Z" {
        return FixedOffset::east_opt(0);
    }
    let (&sign, digits) = bytes.split_first()?;
    let sign = match sign {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let h = parse_two_digits(digits)?;
    let (m, s) = match digits.len() {
        2 => (0, 0),
        4 => (parse_two_digits(&digits[2..])?, 0),
        5 if digits[2] == b':' => (parse_two_digits(&digits[3..])?, 0),
        8 if digits[2] == b':' && digits[5] == b':' => (
            parse_two_digits(&digits[3..])?,
            parse_two_digits(&digits[6..])?,
        ),
        _ => return None,
    };
    if h > 23 || m > 59 || s > 59 {
        return None;
    }
    FixedOffset::east_opt(sign * (i32::from(h) * 3600 + i32::from(m) * 60 + i32::from(s)))
}
