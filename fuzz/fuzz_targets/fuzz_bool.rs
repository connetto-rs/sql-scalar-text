#![no_main]

use libfuzzer_sys::fuzz_target;
use sql_values::parse_bool;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = core::str::from_utf8(data) else {
        return;
    };
    let Some(parsed) = parse_bool(text) else {
        return;
    };
    let spellings = if parsed { ["t", "1"] } else { ["f", "0"] };
    assert!(spellings.contains(&text), "{text:?} parsed as {parsed}");
    for spelling in spellings {
        assert_eq!(
            parse_bool(spelling),
            Some(parsed),
            "{text:?} as {spelling:?}"
        );
    }
});
