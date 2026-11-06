//! PostgreSQL 18's `timezone_abbreviations` files as `tzparser.c` reads them, and the lookup
//! `DecodeTimezoneAbbrev` runs in the table they make. The build script compiles this module
//! too, to bundle PostgreSQL's own sets as static tables.

use alloc::{borrow::Cow, string::String, vec::Vec};

/// `TOKMAXLEN`, the longest abbreviation a set holds and the prefix a lookup compares.
const TOKEN_MAX_LEN: usize = 10;
/// The largest offset `validateTzEntry` takes, 14 hours in seconds.
const MAX_OFFSET: i64 = 14 * 3_600;
/// `fgets`' buffer, which a line must leave a byte of.
const LINE_BUFFER: usize = 1_024;
/// The deepest `@INCLUDE` nesting `ParseTzFile` follows.
const MAX_DEPTH: usize = 3;
/// `WHITESPACE`, the bytes `strtok_r` splits a line at.
const WHITESPACE: &[u8] = b" \t\n\r";

/// What an abbreviation stands for.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum Meaning {
    /// A fixed offset east of UTC in seconds, `TZ` or, for daylight saving time, `DTZ`.
    Fixed { offset: i32, dst: bool },
    /// `DYNTZ`, the meaning the abbreviation has in a time zone at the literal's date.
    Zone(Cow<'static, str>),
}

/// One entry of a set, its abbreviation lowercased as `validateTzEntry` leaves it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct Abbreviation {
    pub(crate) token: Cow<'static, str>,
    pub(crate) meaning: Meaning,
}

/// Why PostgreSQL refuses a time zone abbreviation file, one variant per message `tzparser.c`
/// raises.
#[derive(Debug, Clone, PartialEq, Eq, Hash, thiserror::Error)]
pub enum TimeZoneAbbreviationsError {
    /// A file name with a character that is not a letter.
    #[error("invalid time zone file name \"{file}\"")]
    InvalidFileName {
        /// The file named.
        file: String,
    },
    /// An `@INCLUDE` nested more than three deep.
    #[error("time zone file recursion limit exceeded in file \"{file}\"")]
    RecursionLimit {
        /// The file that would have been read.
        file: String,
    },
    /// A file the caller's files do not have.
    #[error("could not read time zone file \"{file}\"")]
    FileNotFound {
        /// The file named.
        file: String,
    },
    /// A line of 1023 bytes or more.
    #[error("line is too long in time zone file \"{file}\", line {line}")]
    LineTooLong {
        /// The file holding the line.
        file: String,
        /// The line, from 1.
        line: usize,
    },
    /// An `@INCLUDE` naming no file.
    #[error("@INCLUDE without file name in time zone file \"{file}\", line {line}")]
    IncludeWithoutFileName {
        /// The file holding the line.
        file: String,
        /// The line, from 1.
        line: usize,
    },
    /// An abbreviation with nothing after it.
    #[error("missing time zone offset in time zone file \"{file}\", line {line}")]
    MissingOffset {
        /// The file holding the line.
        file: String,
        /// The line, from 1.
        line: usize,
    },
    /// An offset with characters after its digits.
    #[error("invalid number for time zone offset in time zone file \"{file}\", line {line}")]
    InvalidOffset {
        /// The file holding the line.
        file: String,
        /// The line, from 1.
        line: usize,
    },
    /// Something other than a comment after an entry.
    #[error("invalid syntax in time zone file \"{file}\", line {line}")]
    InvalidSyntax {
        /// The file holding the line.
        file: String,
        /// The line, from 1.
        line: usize,
    },
    /// An abbreviation longer than 10 bytes.
    #[error(
        "time zone abbreviation \"{abbreviation}\" is too long (maximum 10 characters) in time zone file \"{file}\", line {line}"
    )]
    AbbreviationTooLong {
        /// The abbreviation as written.
        abbreviation: String,
        /// The file holding the line.
        file: String,
        /// The line, from 1.
        line: usize,
    },
    /// An offset beyond 14 hours either side of UTC.
    #[error("time zone offset {offset} is out of range in time zone file \"{file}\", line {line}")]
    OffsetOutOfRange {
        /// The offset in seconds, as C's `int` holds it.
        offset: i32,
        /// The file holding the line.
        file: String,
        /// The line, from 1.
        line: usize,
    },
    /// An abbreviation defined twice with different meanings and no `@OVERRIDE` before the
    /// second.
    #[error("time zone abbreviation \"{abbreviation}\" is multiply defined")]
    MultiplyDefined {
        /// The abbreviation, lowercased.
        abbreviation: String,
        /// The file holding the second definition.
        file: String,
        /// Its line, from 1.
        line: usize,
    },
}

/// Where an entry was read, for its errors.
struct Place<'a> {
    file: &'a str,
    line: usize,
}

impl Place<'_> {
    fn file(&self) -> String {
        String::from(self.file)
    }
}

/// C's `isspace` in the C locale.
const fn is_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | 0x0B | 0x0C | b'\r')
}

/// `strtok_r` over `WHITESPACE`, the next token and what follows it.
fn next_token(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
    let start = bytes.iter().position(|b| !WHITESPACE.contains(b))?;
    let rest = &bytes[start..];
    let end = rest
        .iter()
        .position(|b| WHITESPACE.contains(b))
        .unwrap_or(rest.len());
    Some((&rest[..end], &rest[end..]))
}

/// C's `strtol` in base 10 over a whole token, saturating on overflow, then the conversion to
/// `int` that keeps its low 32 bits. `None` when characters follow the digits.
fn strtol_int(token: &[u8]) -> Option<i32> {
    let (negative, digits) = match token.split_first() {
        Some((b'-', rest)) => (true, rest),
        Some((b'+', rest)) => (false, rest),
        _ => (false, token),
    };
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    // Past 2^64 every value saturates to `LONG_MIN` or `LONG_MAX` alike.
    let magnitude = digits.iter().fold(0i128, |value, &digit| {
        (value * 10 + i128::from(digit - b'0')).min(1 << 64)
    });
    let signed = if negative { -magnitude } else { magnitude };
    let long = signed.clamp(i128::from(i64::MIN), i128::from(i64::MAX));
    // Intended loss, as C's assignment of a `long` to an `int` keeps its low 32 bits.
    Some(long as i32)
}

/// `splitTzLine` and `validateTzEntry`, the entry a line defines.
fn split_line(line: &[u8], place: &Place<'_>) -> Result<Abbreviation, TimeZoneAbbreviationsError> {
    let (abbreviation, rest) = next_token(line).expect("a line past its blanks has a token");
    let Some((value, rest)) = next_token(rest) else {
        return Err(TimeZoneAbbreviationsError::MissingOffset {
            file: place.file(),
            line: place.line,
        });
    };
    let (meaning, rest) = if matches!(value.first(), Some(b'0'..=b'9' | b'+' | b'-')) {
        let offset =
            strtol_int(value).ok_or_else(|| TimeZoneAbbreviationsError::InvalidOffset {
                file: place.file(),
                line: place.line,
            })?;
        match next_token(rest) {
            Some((flag, after)) if flag.eq_ignore_ascii_case(b"D") => {
                (Meaning::Fixed { offset, dst: true }, after)
            }
            _ => (Meaning::Fixed { offset, dst: false }, rest),
        }
    } else {
        let zone = String::from_utf8_lossy(value).into_owned();
        (Meaning::Zone(Cow::Owned(zone)), rest)
    };
    if next_token(rest).is_some_and(|(remain, _)| remain[0] != b'#') {
        return Err(TimeZoneAbbreviationsError::InvalidSyntax {
            file: place.file(),
            line: place.line,
        });
    }
    let written = String::from_utf8_lossy(abbreviation).into_owned();
    if abbreviation.len() > TOKEN_MAX_LEN {
        return Err(TimeZoneAbbreviationsError::AbbreviationTooLong {
            abbreviation: written,
            file: place.file(),
            line: place.line,
        });
    }
    if let Meaning::Fixed { offset, .. } = meaning
        && !(-MAX_OFFSET..=MAX_OFFSET).contains(&i64::from(offset))
    {
        return Err(TimeZoneAbbreviationsError::OffsetOutOfRange {
            offset,
            file: place.file(),
            line: place.line,
        });
    }
    Ok(Abbreviation {
        token: Cow::Owned(written.to_ascii_lowercase()),
        meaning,
    })
}

/// `addToArray`, inserting `entry` in token order, keeping an identical earlier entry, and
/// replacing a different one only after `@OVERRIDE`.
fn add(
    entries: &mut Vec<Abbreviation>,
    entry: Abbreviation,
    replace: bool,
    place: &Place<'_>,
) -> Result<(), TimeZoneAbbreviationsError> {
    match entries.binary_search_by(|probe| probe.token.as_bytes().cmp(entry.token.as_bytes())) {
        Ok(at) if entries[at].meaning == entry.meaning => Ok(()),
        Ok(at) if replace => {
            entries[at].meaning = entry.meaning;
            Ok(())
        }
        Ok(_) => Err(TimeZoneAbbreviationsError::MultiplyDefined {
            abbreviation: String::from(entry.token),
            file: place.file(),
            line: place.line,
        }),
        Err(at) => {
            entries.insert(at, entry);
            Ok(())
        }
    }
}

/// `ParseTzFile`, reading the file `name` the caller's `files` hold into `entries`.
pub(crate) fn parse_file<'f>(
    name: &str,
    depth: usize,
    files: &mut dyn FnMut(&str) -> Option<&'f str>,
    entries: &mut Vec<Abbreviation>,
) -> Result<(), TimeZoneAbbreviationsError> {
    if !name.bytes().all(|b| b.is_ascii_alphabetic()) {
        return Err(TimeZoneAbbreviationsError::InvalidFileName {
            file: String::from(name),
        });
    }
    if depth > MAX_DEPTH {
        return Err(TimeZoneAbbreviationsError::RecursionLimit {
            file: String::from(name),
        });
    }
    let text = files(name).ok_or_else(|| TimeZoneAbbreviationsError::FileNotFound {
        file: String::from(name),
    })?;
    let mut replace = false;
    for (index, raw) in text.as_bytes().split_inclusive(|&b| b == b'\n').enumerate() {
        let place = Place {
            file: name,
            line: index + 1,
        };
        if raw.len() >= LINE_BUFFER - 1 {
            return Err(TimeZoneAbbreviationsError::LineTooLong {
                file: place.file(),
                line: place.line,
            });
        }
        let start = raw.iter().take_while(|&&b| is_space(b)).count();
        let line = &raw[start..];
        if line.is_empty() || line[0] == b'#' {
            continue;
        }
        let directive = |word: &[u8]| {
            line.get(..word.len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case(word))
        };
        if directive(b"@INCLUDE") {
            let Some((include, _)) = next_token(&line[8..]) else {
                return Err(TimeZoneAbbreviationsError::IncludeWithoutFileName {
                    file: place.file(),
                    line: place.line,
                });
            };
            let include = String::from_utf8_lossy(include);
            parse_file(&include, depth + 1, files, entries)?;
            continue;
        }
        if directive(b"@OVERRIDE") {
            replace = true;
            continue;
        }
        let entry = split_line(line, &place)?;
        add(entries, entry, replace, &place)?;
    }
    Ok(())
}

/// `datebsearch` with `strncmp` over `TOKMAXLEN` bytes, so that a key longer than ten bytes
/// matches the ten-byte abbreviation it starts with.
pub(crate) fn search<'e>(entries: &'e [Abbreviation], key: &[u8]) -> Option<&'e Abbreviation> {
    let key = &key[..key.len().min(TOKEN_MAX_LEN)];
    entries
        .binary_search_by(|probe| probe.token.as_bytes().cmp(key))
        .ok()
        .map(|at| &entries[at])
}
