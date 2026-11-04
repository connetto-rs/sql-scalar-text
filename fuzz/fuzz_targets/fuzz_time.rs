#![no_main]

#[path = "time_spellings.rs"]
mod time_spellings;

use libfuzzer_sys::fuzz_target;
use sql_values::parse_time;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = core::str::from_utf8(data) else {
        return;
    };
    let Some(parsed) = parse_time(text) else {
        return;
    };
    for spelling in time_spellings::time_spellings(parsed) {
        assert_eq!(
            parse_time(&spelling),
            Some(parsed),
            "{text:?} as {spelling:?}"
        );
    }
});
