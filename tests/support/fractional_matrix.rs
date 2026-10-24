use chrono::NaiveDate;
use sql_scalar_text::parse_timestamp;

/// Assert each case's text parses as 2024-01-15 10:30:45 plus `frac_ns` nanoseconds.
pub fn assert_fractional_seconds(cases: &[(&str, u32)], mut fetch: impl FnMut(&str) -> String) {
    for &(expr, frac_ns) in cases {
        let text = fetch(expr);
        let expected = NaiveDate::from_ymd_opt(2024, 1, 15)
            .unwrap()
            .and_hms_nano_opt(10, 30, 45, frac_ns)
            .unwrap();
        let got = parse_timestamp(&text)
            .unwrap_or_else(|| panic!("parse_timestamp: {text:?} (expr={expr})"));
        assert_eq!(got, expected, "expr={expr} text={text:?}");
    }
}
