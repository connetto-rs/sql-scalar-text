#![no_main]

use libfuzzer_sys::fuzz_target;
use sql_values::parse_pg_bytea_hex;

/// Decode through `u8::from_str_radix`, independently of the crate's nibble table.
fn reference(text: &str) -> Option<Vec<u8>> {
    let hex = text.strip_prefix(r"\x")?;
    if !hex.len().is_multiple_of(2) || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
        .collect()
}

fuzz_target!(|data: &[u8]| {
    let Ok(text) = core::str::from_utf8(data) else {
        return;
    };
    let parsed = parse_pg_bytea_hex(text);
    assert_eq!(parsed, reference(text), "{text:?}");
    let Some(bytes) = parsed else {
        return;
    };
    let lower: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    for spelling in [
        format!(r"\x{lower}"),
        format!(r"\x{}", lower.to_uppercase()),
    ] {
        assert_eq!(
            parse_pg_bytea_hex(&spelling).as_ref(),
            Some(&bytes),
            "{text:?} as {spelling:?}"
        );
    }
});
