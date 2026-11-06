//! PostgreSQL 18's `ParseDateTime`, which splits a date, time or interval literal into typed
//! tokens, and the keyword table and number readers `datetime.c` shares between its decoders.

use crate::float::{round_ties_even, truncate_to_i32};

/// `DTERR_BAD_FORMAT`, a literal that is not one PostgreSQL reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BadFormat;

/// The largest `workbuf` a caller hands `ParseDateTime`, `interval_in`'s 256 bytes.
pub(crate) const MAX_WORKBUF_LEN: usize = 256;

/// `MAXDATEFIELDS`, the most tokens `ParseDateTime` splits a literal into.
pub(crate) const MAX_DATE_FIELDS: usize = 25;
/// `TOKMAXLEN`, the prefix a keyword lookup compares.
pub(crate) const TOKEN_MAX_LEN: usize = 10;

// Token types, as `datetime.h` numbers them. A field type doubles as a bit
// position in the masks that catch a field written twice.
pub(crate) const RESERV: i32 = 0;
pub(crate) const MONTH: i32 = 1;
pub(crate) const YEAR: i32 = 2;
pub(crate) const DAY: i32 = 3;
pub(crate) const TZ: i32 = 5;
pub(crate) const DTZ: i32 = 6;
pub(crate) const DYNTZ: i32 = 7;
pub(crate) const IGNORE_DTF: i32 = 8;
pub(crate) const AMPM: i32 = 9;
pub(crate) const HOUR: i32 = 10;
pub(crate) const MINUTE: i32 = 11;
pub(crate) const SECOND: i32 = 12;
pub(crate) const MILLISECOND: i32 = 13;
pub(crate) const MICROSECOND: i32 = 14;
pub(crate) const DOY: i32 = 15;
pub(crate) const DOW: i32 = 16;
pub(crate) const UNITS: i32 = 17;
pub(crate) const ADBC: i32 = 18;
pub(crate) const AGO: i32 = 19;
pub(crate) const ISOTIME: i32 = 23;
pub(crate) const WEEK: i32 = 24;
pub(crate) const DECADE: i32 = 25;
pub(crate) const CENTURY: i32 = 26;
pub(crate) const MILLENNIUM: i32 = 27;
pub(crate) const DTZMOD: i32 = 28;
pub(crate) const UNKNOWN_FIELD: i32 = 31;

// Unit and keyword codes, as `datetime.h` numbers them.
pub(crate) const DTK_DATE: i32 = 2;
pub(crate) const DTK_TIME: i32 = 3;
pub(crate) const DTK_TZ: i32 = 4;
pub(crate) const DTK_EARLY: i32 = 9;
pub(crate) const DTK_LATE: i32 = 10;
pub(crate) const DTK_EPOCH: i32 = 11;
pub(crate) const DTK_NOW: i32 = 12;
pub(crate) const DTK_YESTERDAY: i32 = 13;
pub(crate) const DTK_TODAY: i32 = 14;
pub(crate) const DTK_TOMORROW: i32 = 15;
pub(crate) const DTK_ZULU: i32 = 16;
pub(crate) const DTK_DELTA: i32 = 17;
pub(crate) const DTK_SECOND: i32 = 18;
pub(crate) const DTK_MINUTE: i32 = 19;
pub(crate) const DTK_HOUR: i32 = 20;
pub(crate) const DTK_DAY: i32 = 21;
pub(crate) const DTK_WEEK: i32 = 22;
pub(crate) const DTK_MONTH: i32 = 23;
pub(crate) const DTK_QUARTER: i32 = 24;
pub(crate) const DTK_YEAR: i32 = 25;
pub(crate) const DTK_DECADE: i32 = 26;
pub(crate) const DTK_CENTURY: i32 = 27;
pub(crate) const DTK_MILLENNIUM: i32 = 28;
pub(crate) const DTK_MILLISEC: i32 = 29;
pub(crate) const DTK_MICROSEC: i32 = 30;
pub(crate) const DTK_JULIAN: i32 = 31;
pub(crate) const DTK_DOW: i32 = 32;
pub(crate) const DTK_DOY: i32 = 33;
pub(crate) const DTK_TZ_HOUR: i32 = 34;
pub(crate) const DTK_TZ_MINUTE: i32 = 35;
pub(crate) const DTK_ISOYEAR: i32 = 36;
pub(crate) const DTK_ISODOW: i32 = 37;

pub(crate) const fn dtk_m(field: i32) -> u32 {
    1 << field
}

pub(crate) const DTK_ALL_SECS_M: u32 = dtk_m(SECOND) | dtk_m(MILLISECOND) | dtk_m(MICROSECOND);
pub(crate) const DTK_DATE_M: u32 = dtk_m(YEAR) | dtk_m(MONTH) | dtk_m(DAY);
pub(crate) const DTK_TIME_M: u32 = dtk_m(HOUR) | dtk_m(MINUTE) | DTK_ALL_SECS_M;

/// `datetktbl`, the date keywords, consulted for a word that names no unit.
pub(crate) const DATE_TOKENS: &[(&str, i32, i32)] = &[
    ("+infinity", RESERV, DTK_LATE),
    ("-infinity", RESERV, DTK_EARLY),
    ("ad", ADBC, 0),
    ("allballs", RESERV, DTK_ZULU),
    ("am", AMPM, 0),
    ("apr", MONTH, 4),
    ("april", MONTH, 4),
    ("at", IGNORE_DTF, 0),
    ("aug", MONTH, 8),
    ("august", MONTH, 8),
    ("bc", ADBC, 1),
    ("d", UNITS, DTK_DAY),
    ("dec", MONTH, 12),
    ("december", MONTH, 12),
    ("dow", UNITS, DTK_DOW),
    ("doy", UNITS, DTK_DOY),
    ("dst", DTZMOD, 3_600),
    ("epoch", RESERV, DTK_EPOCH),
    ("feb", MONTH, 2),
    ("february", MONTH, 2),
    ("fri", DOW, 5),
    ("friday", DOW, 5),
    ("h", UNITS, DTK_HOUR),
    ("infinity", RESERV, DTK_LATE),
    ("isodow", UNITS, DTK_ISODOW),
    ("isoyear", UNITS, DTK_ISOYEAR),
    ("j", UNITS, DTK_JULIAN),
    ("jan", MONTH, 1),
    ("january", MONTH, 1),
    ("jd", UNITS, DTK_JULIAN),
    ("jul", MONTH, 7),
    ("julian", UNITS, DTK_JULIAN),
    ("july", MONTH, 7),
    ("jun", MONTH, 6),
    ("june", MONTH, 6),
    ("m", UNITS, DTK_MONTH),
    ("mar", MONTH, 3),
    ("march", MONTH, 3),
    ("may", MONTH, 5),
    ("mm", UNITS, DTK_MINUTE),
    ("mon", DOW, 1),
    ("monday", DOW, 1),
    ("nov", MONTH, 11),
    ("november", MONTH, 11),
    ("now", RESERV, DTK_NOW),
    ("oct", MONTH, 10),
    ("october", MONTH, 10),
    ("on", IGNORE_DTF, 0),
    ("pm", AMPM, 1),
    ("s", UNITS, DTK_SECOND),
    ("sat", DOW, 6),
    ("saturday", DOW, 6),
    ("sep", MONTH, 9),
    ("sept", MONTH, 9),
    ("september", MONTH, 9),
    ("sun", DOW, 0),
    ("sunday", DOW, 0),
    ("t", ISOTIME, DTK_TIME),
    ("thu", DOW, 4),
    ("thur", DOW, 4),
    ("thurs", DOW, 4),
    ("thursday", DOW, 4),
    ("today", RESERV, DTK_TODAY),
    ("tomorrow", RESERV, DTK_TOMORROW),
    ("tue", DOW, 2),
    ("tues", DOW, 2),
    ("tuesday", DOW, 2),
    ("wed", DOW, 3),
    ("wednesday", DOW, 3),
    ("weds", DOW, 3),
    ("y", UNITS, DTK_YEAR),
    ("yesterday", RESERV, DTK_YESTERDAY),
];

/// The field types `ParseDateTime` assigns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FieldType {
    /// `DTK_NUMBER`, digits with at most a decimal point.
    Number,
    /// `DTK_STRING`, a word.
    String,
    /// `DTK_DATE`, digits with delimiters or a word run into digits.
    Date,
    /// `DTK_TIME`, digits with colons.
    Time,
    /// `DTK_TZ`, a sign followed by digits.
    Tz,
    /// `DTK_SPECIAL`, a sign followed by a word.
    Special,
}

/// A `ParseDateTime` token, located in the work buffer.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Field {
    start: usize,
    end: usize,
    kind: FieldType,
}

/// A caller's `workbuf` and the tokens `ParseDateTime` wrote into it.
pub(crate) struct Tokens {
    bytes: [u8; MAX_WORKBUF_LEN],
    /// The caller's buffer size, which bounds the tokens' bytes.
    limit: usize,
    used: usize,
    fields: [Field; MAX_DATE_FIELDS],
    count: usize,
}

impl Tokens {
    /// Appends one byte to the token being written, refusing a literal whose
    /// tokens overrun the work buffer.
    fn push(&mut self, byte: u8) -> Result<(), BadFormat> {
        if self.used + 1 >= self.limit {
            return Err(BadFormat);
        }
        self.bytes[self.used] = byte;
        self.used += 1;
        Ok(())
    }

    /// Closes the token that started at `start`, which takes its terminating
    /// NUL in the work buffer.
    fn finish(&mut self, start: usize, kind: FieldType) {
        self.fields[self.count] = Field {
            start,
            end: self.used,
            kind,
        };
        self.count += 1;
        self.used += 1;
    }

    /// How many tokens `ParseDateTime` wrote.
    pub(crate) const fn len(&self) -> usize {
        self.count
    }

    pub(crate) fn field(&self, index: usize) -> (&[u8], FieldType) {
        let field = self.fields[index];
        (&self.bytes[field.start..field.end], field.kind)
    }
}

/// `ParseDateTime`, which splits a literal into typed tokens within a `workbuf` of
/// `buffer_len` bytes.
pub(crate) fn parse_date_time(input: &[u8], buffer_len: usize) -> Result<Tokens, BadFormat> {
    debug_assert!(
        buffer_len <= MAX_WORKBUF_LEN,
        "a caller's workbuf fits the tokens' array"
    );
    let mut tokens = Tokens {
        bytes: [0; MAX_WORKBUF_LEN],
        limit: buffer_len,
        used: 0,
        fields: [Field {
            start: 0,
            end: 0,
            kind: FieldType::Number,
        }; MAX_DATE_FIELDS],
        count: 0,
    };
    let mut at = 0_usize;
    let byte = |index: usize| input.get(index).copied().unwrap_or(0);

    while at < input.len() {
        if is_space(byte(at)) {
            at += 1;
            continue;
        }
        if tokens.count >= MAX_DATE_FIELDS {
            return Err(BadFormat);
        }
        let start = tokens.used;
        let kind;
        let current = byte(at);
        if current.is_ascii_digit() {
            tokens.push(current)?;
            at += 1;
            while byte(at).is_ascii_digit() {
                tokens.push(byte(at))?;
                at += 1;
            }
            if byte(at) == b':' {
                kind = FieldType::Time;
                tokens.push(b':')?;
                at += 1;
                while byte(at).is_ascii_digit() || byte(at) == b':' || byte(at) == b'.' {
                    tokens.push(byte(at))?;
                    at += 1;
                }
            } else if matches!(byte(at), b'-' | b'/' | b'.') {
                let delimiter = byte(at);
                tokens.push(delimiter)?;
                at += 1;
                if byte(at).is_ascii_digit() {
                    let mut date_kind = if delimiter == b'.' {
                        FieldType::Number
                    } else {
                        FieldType::Date
                    };
                    while byte(at).is_ascii_digit() {
                        tokens.push(byte(at))?;
                        at += 1;
                    }
                    if byte(at) == delimiter {
                        date_kind = FieldType::Date;
                        tokens.push(delimiter)?;
                        at += 1;
                        while byte(at).is_ascii_digit() || byte(at) == delimiter {
                            tokens.push(byte(at))?;
                            at += 1;
                        }
                    }
                    kind = date_kind;
                } else {
                    kind = FieldType::Date;
                    while byte(at).is_ascii_alphanumeric() || byte(at) == delimiter {
                        tokens.push(byte(at).to_ascii_lowercase())?;
                        at += 1;
                    }
                }
            } else {
                kind = FieldType::Number;
            }
        } else if current == b'.' {
            tokens.push(b'.')?;
            at += 1;
            while byte(at).is_ascii_digit() {
                tokens.push(byte(at))?;
                at += 1;
            }
            kind = FieldType::Number;
        } else if current.is_ascii_alphabetic() {
            tokens.push(current.to_ascii_lowercase())?;
            at += 1;
            while byte(at).is_ascii_alphabetic() {
                tokens.push(byte(at).to_ascii_lowercase())?;
                at += 1;
            }
            // A word followed by a digit or a sign is a date or a zone name
            // unless it is one of the date keywords.
            let is_date = matches!(byte(at), b'-' | b'/' | b'.')
                || ((byte(at) == b'+' || byte(at).is_ascii_digit())
                    && lookup(DATE_TOKENS, &tokens.bytes[start..tokens.used]).is_none());
            if is_date {
                loop {
                    tokens.push(byte(at).to_ascii_lowercase())?;
                    at += 1;
                    let next = byte(at);
                    if !(matches!(next, b'+' | b'-' | b'/' | b'_' | b'.' | b':')
                        || next.is_ascii_alphanumeric())
                    {
                        break;
                    }
                }
                kind = FieldType::Date;
            } else {
                kind = FieldType::String;
            }
        } else if current == b'+' || current == b'-' {
            tokens.push(current)?;
            at += 1;
            while is_space(byte(at)) {
                at += 1;
            }
            if byte(at).is_ascii_digit() {
                tokens.push(byte(at))?;
                at += 1;
                while byte(at).is_ascii_digit() || matches!(byte(at), b':' | b'.' | b'-') {
                    tokens.push(byte(at))?;
                    at += 1;
                }
                kind = FieldType::Tz;
            } else if byte(at).is_ascii_alphabetic() {
                tokens.push(byte(at).to_ascii_lowercase())?;
                at += 1;
                while byte(at).is_ascii_alphabetic() {
                    tokens.push(byte(at).to_ascii_lowercase())?;
                    at += 1;
                }
                kind = FieldType::Special;
            } else {
                return Err(BadFormat);
            }
        } else if current.is_ascii_punctuation() {
            at += 1;
            continue;
        } else {
            return Err(BadFormat);
        }
        tokens.finish(start, kind);
    }
    Ok(tokens)
}

/// `isspace` in the C locale.
pub(crate) fn is_space(character: u8) -> bool {
    matches!(character, b' ' | b'\t' | b'\n' | 0x0B | 0x0C | b'\r')
}

/// `datebsearch`, matching `key` against each token's first `TOKMAXLEN` bytes.
pub(crate) fn lookup(table: &[(&str, i32, i32)], key: &[u8]) -> Option<(i32, i32)> {
    let key = &key[..key.len().min(TOKEN_MAX_LEN)];
    table.iter().find_map(|&(token, kind, value)| {
        let token = token.as_bytes();
        (&token[..token.len().min(TOKEN_MAX_LEN)] == key).then_some((kind, value))
    })
}

/// `strtol` over a token, answering the value, where the digits end, and whether C reports
/// `ERANGE`.
pub(crate) fn strtoll(text: &[u8], start: usize) -> (i64, usize, bool) {
    let mut at = start;
    let negative = match text.get(at) {
        Some(b'-') => {
            at += 1;
            true
        }
        Some(b'+') => {
            at += 1;
            false
        }
        _ => false,
    };
    let digits_start = at;
    let mut value: i64 = 0;
    let mut overflowed = false;
    while let Some(digit) = text.get(at).filter(|byte| byte.is_ascii_digit()) {
        let digit = i64::from(digit - b'0');
        let next = value.checked_mul(10).and_then(|scaled| {
            if negative {
                scaled.checked_sub(digit)
            } else {
                scaled.checked_add(digit)
            }
        });
        if let Some(next) = next {
            value = next;
        } else {
            overflowed = true;
            value = if negative { i64::MIN } else { i64::MAX };
        }
        at += 1;
    }
    if at == digits_start {
        return (0, start, false);
    }
    (value, at, overflowed)
}

/// `strtoint`, which is `strtol` narrowed to `int` with `ERANGE` when it does not fit.
pub(crate) fn strtoint(text: &[u8], start: usize) -> (i32, usize, bool) {
    let (value, end, overflowed) = strtoll(text, start);
    match i32::try_from(value) {
        Ok(narrow) => (narrow, end, overflowed),
        // PostgreSQL returns the truncated value alongside `ERANGE`, and every
        // caller fails on the `ERANGE` before reading it.
        Err(_) => (0, end, true),
    }
}

/// `ParseFraction`, the value of the `.digits` ending a token.
pub(crate) fn parse_fraction(text: &[u8]) -> Result<f64, BadFormat> {
    debug_assert_eq!(text.first(), Some(&b'.'), "a fraction starts at its point");
    if text.len() == 1 {
        return Ok(0.0);
    }
    if !text[1..].iter().all(u8::is_ascii_digit) {
        return Err(BadFormat);
    }
    core::str::from_utf8(text)
        .ok()
        .and_then(|digits| digits.parse::<f64>().ok())
        .ok_or(BadFormat)
}

/// `ParseFractionalSecond`, a fraction as whole microseconds.
pub(crate) fn parse_fractional_second(text: &[u8]) -> Result<i32, BadFormat> {
    let fraction = parse_fraction(text)?;
    Ok(truncate_to_i32(round_ties_even(fraction * 1_000_000.0)))
}
