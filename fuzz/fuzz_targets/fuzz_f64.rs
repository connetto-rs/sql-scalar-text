#![no_main]

use libfuzzer_sys::fuzz_target;
use sql_values::parse_f64;

/// Rust's shortest exponent form, and its mantissa with the exponent PostgreSQL and SQLite print.
fn exponent_spellings(value: f64, spellings: &mut Vec<String>) {
    let rust = format!("{value:e}");
    let (mantissa, exponent) = rust.split_once('e').expect("LowerExp has an exponent");
    let exponent: i32 = exponent.parse().expect("LowerExp exponent is an integer");
    let sign = if exponent < 0 { '-' } else { '+' };
    let exponent = exponent.unsigned_abs();
    spellings.push(format!("{mantissa}e{sign}{exponent:02}"));
    if !mantissa.contains('.') {
        spellings.push(format!("{mantissa}.0e{sign}{exponent:02}"));
    }
    spellings.push(rust);
}

fuzz_target!(|data: &[u8]| {
    let Ok(text) = core::str::from_utf8(data) else {
        return;
    };
    let Some(parsed) = parse_f64(text) else {
        return;
    };
    let spellings: Vec<String> = if parsed.is_nan() {
        vec!["NaN".into()]
    } else if parsed.is_infinite() {
        let sign = if parsed < 0.0 { "-" } else { "" };
        vec![format!("{sign}Infinity"), format!("{sign}Inf")]
    } else {
        let mut spellings = vec![format!("{parsed}")];
        exponent_spellings(parsed, &mut spellings);
        spellings
    };
    for spelling in spellings {
        let reparsed = parse_f64(&spelling).unwrap_or_else(|| panic!("{text:?} as {spelling:?}"));
        if parsed.is_nan() {
            assert!(reparsed.is_nan(), "{text:?} as {spelling:?}");
        } else {
            assert_eq!(
                reparsed.to_bits(),
                parsed.to_bits(),
                "{text:?} as {spelling:?}"
            );
        }
    }
});
