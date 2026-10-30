#![no_main]

use libfuzzer_sys::fuzz_target;
use sql_scalar_text::{
    IntervalFields, IntervalTypmod, PgInterval, parse_interval, parse_interval_typmod,
};

const FIELDS: [Option<IntervalFields>; 14] = [
    None,
    Some(IntervalFields::Year),
    Some(IntervalFields::Month),
    Some(IntervalFields::Day),
    Some(IntervalFields::Hour),
    Some(IntervalFields::Minute),
    Some(IntervalFields::Second),
    Some(IntervalFields::YearToMonth),
    Some(IntervalFields::DayToHour),
    Some(IntervalFields::DayToMinute),
    Some(IntervalFields::DayToSecond),
    Some(IntervalFields::HourToMinute),
    Some(IntervalFields::HourToSecond),
    Some(IntervalFields::MinuteToSecond),
];

/// The printed text reads back as the same three fields, except the clock
/// PostgreSQL prints for `i64::MIN` microseconds, which it cannot read back.
fn assert_reprints(text: &str, parsed: PgInterval) {
    if parsed.is_finite() && parsed.microseconds == i64::MIN {
        return;
    }
    let printed = parsed.to_string();
    let reread = parse_interval(&printed)
        .unwrap_or_else(|| panic!("{text:?} printed as {printed:?}, which does not parse"));
    assert_eq!(
        (reread.months, reread.days, reread.microseconds),
        (parsed.months, parsed.days, parsed.microseconds),
        "{text:?} printed as {printed:?}"
    );
}

fuzz_target!(|data: &[u8]| {
    let Ok(text) = core::str::from_utf8(data) else {
        return;
    };
    if let Some(parsed) = parse_interval(text) {
        assert_reprints(text, parsed);
    }
    // The length picks a typmod, so no input byte is spent choosing one.
    let fields = FIELDS[text.len() % FIELDS.len()];
    let precision = u8::try_from(text.len() / FIELDS.len() % 8)
        .ok()
        .filter(|&p| p < 7);
    if let Some(typmod) = IntervalTypmod::new(fields, precision)
        && let Some(parsed) = parse_interval_typmod(text, typmod)
    {
        assert_reprints(text, parsed);
    }
});
