#![no_main]

#[path = "date_spellings.rs"]
mod date_spellings;

use libfuzzer_sys::fuzz_target;
use sql_values::parse_date;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = core::str::from_utf8(data) else {
        return;
    };
    let Some(parsed) = parse_date(text) else {
        return;
    };
    for (body, era) in date_spellings::date_spellings(parsed) {
        let spelling = format!("{body}{era}");
        assert_eq!(
            parse_date(&spelling),
            Some(parsed),
            "{text:?} as {spelling:?}"
        );
    }
});
