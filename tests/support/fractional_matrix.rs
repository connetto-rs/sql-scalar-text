use sql_values::{PgDate, PgTime, PgTimestamp, parse_timestamp};

/// Assert each case's text parses as 2024-01-15 10:30:45 plus `frac_ns` nanoseconds.
pub fn assert_fractional_seconds(cases: &[(&str, u32)], mut fetch: impl FnMut(&str) -> String) {
    for &(expr, frac_ns) in cases {
        let text = fetch(expr);
        let expected = PgTimestamp::new(
            PgDate::from_ymd(2024, 1, 15).unwrap(),
            PgTime::from_hms_micro(10, 30, 45, frac_ns / 1_000).unwrap(),
        )
        .unwrap();
        let got = parse_timestamp(&text)
            .unwrap_or_else(|| panic!("parse_timestamp: {text:?} (expr={expr})"));
        assert_eq!(got, expected, "expr={expr} text={text:?}");
    }
}
