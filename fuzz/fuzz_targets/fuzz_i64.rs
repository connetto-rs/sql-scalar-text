#![no_main]

use libfuzzer_sys::fuzz_target;
use sql_values::parse_i64;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = core::str::from_utf8(data) else {
        return;
    };
    let Some(parsed) = parse_i64(text) else {
        return;
    };
    let spelling = parsed.to_string();
    assert_eq!(
        parse_i64(&spelling),
        Some(parsed),
        "{text:?} as {spelling:?}"
    );
});
