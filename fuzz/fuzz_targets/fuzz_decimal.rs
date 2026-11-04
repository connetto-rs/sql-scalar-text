#![no_main]

use libfuzzer_sys::fuzz_target;
use sql_values::parse_decimal;

/// Plain spellings grow with the scale and reparse in superlinear time, so wider values skip them.
const MAX_PLAIN_SCALE: u64 = 1024;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = core::str::from_utf8(data) else {
        return;
    };
    let Some(parsed) = parse_decimal(text) else {
        return;
    };
    let exact = parsed.as_bigint_and_exponent();
    let (digits, scale) = &exact;

    let scientific = format!("{digits}e{}", -i128::from(*scale));
    let reparsed =
        parse_decimal(&scientific).unwrap_or_else(|| panic!("{text:?} as {scientific:?}"));
    assert_eq!(
        reparsed.as_bigint_and_exponent(),
        exact,
        "{text:?} as {scientific:?}"
    );

    let display = parsed.to_string();
    let reparsed = parse_decimal(&display).unwrap_or_else(|| panic!("{text:?} as {display:?}"));
    assert_eq!(reparsed, parsed, "{text:?} as {display:?}");

    if scale.unsigned_abs() <= MAX_PLAIN_SCALE {
        // the plain spelling engines print keeps a non-negative scale digit for digit
        let plain = parsed.to_plain_string();
        let reparsed = parse_decimal(&plain).unwrap_or_else(|| panic!("{text:?} as {plain:?}"));
        if *scale >= 0 {
            assert_eq!(
                reparsed.as_bigint_and_exponent(),
                exact,
                "{text:?} as {plain:?}"
            );
        } else {
            assert_eq!(reparsed, parsed, "{text:?} as {plain:?}");
        }
    }
});
