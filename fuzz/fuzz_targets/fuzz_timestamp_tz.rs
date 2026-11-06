#![no_main]

#[path = "date_spellings.rs"]
mod date_spellings;
#[path = "time_spellings.rs"]
mod time_spellings;

use libfuzzer_sys::fuzz_target;
use sql_values::{parse_timestamp, parse_timestamp_tz, PgTimestamp};

/// Zero, whole-hour, half-hour and local-mean-time offsets, the last as PostgreSQL prints it.
const OFFSETS: [i32; 5] = [
    0,
    14 * 3600,
    -11 * 3600,
    5 * 3600 + 30 * 60,
    -(4 * 3600 + 56 * 60 + 2),
];

/// Every accepted spelling of an offset of `seconds` east of UTC.
fn offset_spellings(seconds: i32) -> Vec<String> {
    let sign = if seconds < 0 { '-' } else { '+' };
    let abs = seconds.unsigned_abs();
    let (h, m, s) = (abs / 3600, abs / 60 % 60, abs % 60);
    let mut spellings = vec![format!("{sign}{h:02}:{m:02}:{s:02}")];
    if s == 0 {
        spellings.push(format!("{sign}{h:02}{m:02}"));
        spellings.push(format!("{sign}{h:02}:{m:02}"));
        if m == 0 {
            spellings.push(format!("{sign}{h:02}"));
        }
    }
    if seconds == 0 {
        spellings.push("Z".to_string());
    }
    spellings
}

fuzz_target!(|data: &[u8]| {
    let Ok(text) = core::str::from_utf8(data) else {
        return;
    };
    let Some(parsed) = parse_timestamp_tz(text) else {
        return;
    };
    for seconds in OFFSETS {
        let Some(shifted) = parsed.pg_micros().checked_add(i64::from(seconds) * 1_000_000) else {
            continue;
        };
        let Some(local) = PgTimestamp::from_pg_micros(shifted) else {
            continue;
        };
        // An infinity has no local clock to spell.
        let Some(time) = local.time() else {
            continue;
        };
        let date = local.date();
        let offsets = offset_spellings(seconds);
        let times = time_spellings::time_spellings(time);
        for (date_body, era) in date_spellings::date_spellings(date) {
            for t in &times {
                let naive = format!("{date_body} {t}{era}");
                assert_eq!(
                    parse_timestamp(&naive),
                    Some(local),
                    "{text:?} local {naive:?}"
                );
                for separator in [' ', 'T'] {
                    for zone in &offsets {
                        let spelling = format!("{date_body}{separator}{t}{zone}{era}");
                        assert_eq!(
                            parse_timestamp_tz(&spelling),
                            Some(parsed),
                            "{text:?} as {spelling:?}"
                        );
                    }
                }
            }
        }
    }
});
