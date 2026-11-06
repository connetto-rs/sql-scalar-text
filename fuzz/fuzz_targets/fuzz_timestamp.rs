#![no_main]

#[path = "date_spellings.rs"]
mod date_spellings;
#[path = "time_spellings.rs"]
mod time_spellings;

use libfuzzer_sys::fuzz_target;
use sql_values::{parse_date, parse_time, parse_timestamp};

fuzz_target!(|data: &[u8]| {
    let Ok(text) = core::str::from_utf8(data) else {
        return;
    };
    let Some(parsed) = parse_timestamp(text) else {
        return;
    };
    // `infinity` has no date or clock to spell.
    let Some(parsed_time) = parsed.time() else {
        return;
    };
    let times = time_spellings::time_spellings(parsed_time);
    for time in &times {
        assert_eq!(
            parse_time(time),
            Some(parsed_time),
            "{text:?} time {time:?}"
        );
    }
    for (date, era) in date_spellings::date_spellings(parsed.date()) {
        let whole_date = format!("{date}{era}");
        assert_eq!(
            parse_date(&whole_date),
            Some(parsed.date()),
            "{text:?} date {whole_date:?}"
        );
        for separator in [' ', 'T'] {
            for time in &times {
                let spelling = format!("{date}{separator}{time}{era}");
                assert_eq!(
                    parse_timestamp(&spelling),
                    Some(parsed),
                    "{text:?} as {spelling:?}"
                );
            }
        }
    }
});
