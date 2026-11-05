#![no_main]

use libfuzzer_sys::fuzz_target;
use sql_values::{CastTarget, CastValue, CharLength, Literal, NumericTypmod, cast_literal};

/// The `varchar(n)` and `char(n)` length the input's length picks.
fn char_length(len: usize) -> usize {
    len % 5 + 1
}

/// The target the input's length picks, so no input byte is spent choosing one.
fn target(len: usize) -> CastTarget {
    let length = CharLength::new(char_length(len));
    match len % 14 {
        0 => CastTarget::Boolean,
        1 => CastTarget::Smallint,
        2 => CastTarget::Integer,
        3 => CastTarget::Bigint,
        4 => CastTarget::Numeric(None),
        5 => CastTarget::Numeric(NumericTypmod::new(4, 1)),
        6 => CastTarget::Numeric(NumericTypmod::new(2, -1)),
        7 => CastTarget::Text,
        8 => CastTarget::Varchar(length),
        9 => CastTarget::Bpchar(length),
        10 => CastTarget::Date,
        11 => CastTarget::Timestamp(u8::try_from(len % 8).ok()),
        12 => CastTarget::TimestampTz(u8::try_from(len % 8).ok()),
        _ => CastTarget::Bpchar(None),
    }
}

/// A `varchar(n)` value has at most `n` characters and a `char(n)` value exactly `n`, and every
/// other value prints as text that reads back as the same value and the same text.
fn assert_reads_back(text: &str, target: CastTarget, value: &CastValue) {
    let length = char_length(text.len());
    match (target, value) {
        (CastTarget::Varchar(Some(_)), CastValue::Text(clipped)) => {
            assert!(clipped.chars().count() <= length, "{text:?} as {clipped:?}");
        }
        (CastTarget::Bpchar(Some(_)), CastValue::Bpchar(padded)) => {
            assert_eq!(padded.chars().count(), length, "{text:?} as {padded:?}");
        }
        (CastTarget::Text | CastTarget::Varchar(None) | CastTarget::Bpchar(None), _) => {}
        _ => {
            let printed = value.to_string();
            let reread = cast_literal(Literal::Quoted(&printed), target)
                .unwrap_or_else(|error| panic!("{text:?} printed as {printed:?}: {error}"));
            assert_eq!(&reread, value, "{text:?} printed as {printed:?}");
            assert_eq!(
                reread.to_string(),
                printed,
                "{text:?} printed as {printed:?}"
            );
        }
    }
}

fuzz_target!(|data: &[u8]| {
    let Ok(text) = core::str::from_utf8(data) else {
        return;
    };
    let target = target(text.len());
    for literal in [Literal::Quoted(text), Literal::Number(text)] {
        if let Ok(value) = cast_literal(literal, target) {
            assert_reads_back(text, target, &value);
        }
    }
});
