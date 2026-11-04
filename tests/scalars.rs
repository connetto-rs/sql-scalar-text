use sql_values::{parse_bool, parse_f64, parse_i64, parse_pg_bytea_hex};

#[cfg(feature = "decimal")]
use bigdecimal::BigDecimal;
#[cfg(feature = "decimal")]
use core::str::FromStr;
#[cfg(feature = "decimal")]
use sql_values::parse_decimal;

#[test]
fn parses_boolean_spelling_corpus_to_exact_values() {
    for (text, expected) in [("t", true), ("f", false), ("1", true), ("0", false)] {
        assert_eq!(parse_bool(text), Some(expected), "{text}");
    }
}

#[test]
fn parses_bytea_spelling_corpus_to_exact_values() {
    for (text, expected) in [
        (r"\x", Vec::new()),
        (r"\x00", vec![0]),
        (r"\x0001ff", vec![0, 1, 255]),
        (r"\xABCDEF", vec![171, 205, 239]),
    ] {
        assert_eq!(parse_pg_bytea_hex(text), Some(expected), "{text}");
    }
}

#[test]
fn parses_integer_spelling_corpus_to_exact_values() {
    for (text, expected) in [
        ("0", 0),
        ("-1", -1),
        ("+1", 1),
        ("9223372036854775807", i64::MAX),
        ("-9223372036854775808", i64::MIN),
    ] {
        assert_eq!(parse_i64(text), Some(expected), "{text}");
    }
}

#[test]
fn parses_float_spelling_corpus_to_exact_values() {
    for (text, expected) in [
        ("0", 0.0),
        ("-1.5", -1.5),
        ("1e20", 1e20),
        ("3.141592653589793", core::f64::consts::PI),
        ("Infinity", f64::INFINITY),
        ("-Infinity", f64::NEG_INFINITY),
    ] {
        assert_eq!(parse_f64(text), Some(expected), "{text}");
    }
}

#[cfg(feature = "decimal")]
#[test]
fn parses_decimal_spelling_corpus_to_exact_values() {
    for text in ["0", "-1.5", "+1", "1e20", "12345678901234567890.123456789"] {
        let expected = BigDecimal::from_str(text).expect("valid corpus decimal");
        assert_eq!(parse_decimal(text), Some(expected), "{text}");
    }
}

#[test]
fn refuses_scalar_negative_corpus() {
    for text in ["", "true", "false", "nope"] {
        assert_eq!(parse_bool(text), None, "{text}");
    }
    for text in ["", "00", r"\x0", r"\x0g"] {
        assert_eq!(parse_pg_bytea_hex(text), None, "{text}");
    }
    for text in ["", "nope", "9223372036854775808"] {
        assert_eq!(parse_i64(text), None, "{text}");
    }
    for text in ["", "nope"] {
        assert_eq!(parse_f64(text), None, "{text}");
    }
}

#[cfg(feature = "decimal")]
#[test]
fn refuses_decimal_negative_corpus() {
    for text in ["", "nope"] {
        assert_eq!(parse_decimal(text), None, "{text}");
    }
}
