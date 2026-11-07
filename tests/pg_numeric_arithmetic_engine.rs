//! `numeric` arithmetic and rounding computed as PostgreSQL 18 computes them, every answer and
//! every refusal's message measured on the engine.

#[path = "support/answers.rs"]
mod answers;
#[path = "support/containers.rs"]
mod containers;
#[path = "support/exit_slot.rs"]
mod exit_slot;
#[path = "support/postgres.rs"]
mod postgres;

use answers::{Answer, compare, quoted};
use sql_values::PgNumeric;

static PG18: postgres::Slot = exit_slot::ExitSlot::new();

const PG18_TAG: &str = "18.6";

/// Numerics at every sign, display scale, base-10000 digit boundary, both storage bounds and
/// special value.
const OPERANDS: &[&str] = &[
    "0",
    "0.00",
    "-0.000",
    "1",
    "-1",
    "2",
    "3",
    "-7",
    "7.5",
    "-7.5",
    "1.50",
    "2.250",
    "0.5",
    "-0.5",
    "2.5",
    "-2.5",
    "0.001",
    "9.99",
    "-99999.9999",
    "10000",
    "0.0001",
    "12345.6789",
    "3.000000000000000000000",
    "0.333333333333333333333",
    "4294967296",
    // Pairs whose remainder takes the rare correction step of long division in base 10^9.
    "72440598693065722126614242620157263",
    "572136254000000000999999997",
    "635652336469056602798570521201429469",
    "908538600000000003999999996",
    "-18446744073709551616.5",
    "123456789012345678901234567890.123456789",
    "1e100",
    "-1e-100",
    "1e-16383",
    "9.99e-1000",
    "1e131071",
    "-9.9999e131071",
    "NaN",
    "Infinity",
    "-Infinity",
];

/// Scales at and past each clamp `numeric_round` and `numeric_trunc` apply, and around zero.
const SCALES: &[i32] = &[
    i32::MIN,
    -131_074,
    -131_073,
    -131_072,
    -131_071,
    -5,
    -2,
    -1,
    0,
    1,
    2,
    3,
    20,
    16_383,
    16_384,
    i32::MAX,
];

fn numeric(text: &str) -> PgNumeric {
    PgNumeric::parse(text).unwrap_or_else(|error| panic!("{text:?}: {error}"))
}

/// `value` as an SQL `numeric`.
fn sql(value: &str) -> String {
    format!("{}::numeric", quoted(value))
}

#[test]
fn pg18_computes_every_operator_on_every_pair() {
    let mut conn = answers::connect(&PG18, PG18_TAG);
    let mut cases = Vec::new();
    for &left in OPERANDS {
        let (a, l) = (numeric(left), sql(left));
        for &right in OPERANDS {
            let (b, r) = (numeric(right), sql(right));
            for (operator, result) in [
                ("+", a.checked_add(&b)),
                ("-", a.checked_sub(&b)),
                ("*", a.checked_mul(&b)),
                ("/", a.checked_div(&b)),
                ("%", a.checked_rem(&b)),
            ] {
                cases.push((
                    format!("SELECT ({l} {operator} {r})::text"),
                    Answer::of(result),
                ));
            }
        }
    }
    compare(&mut conn, cases);
}

#[test]
fn pg18_negates_and_rounds_every_operand() {
    let mut conn = answers::connect(&PG18, PG18_TAG);
    let mut cases = Vec::new();
    for &text in OPERANDS {
        let (value, operand) = (numeric(text), sql(text));
        cases.push((
            format!("SELECT (-{operand})::text"),
            Answer::Value((-value.clone()).to_string()),
        ));
        cases.push((
            format!("SELECT abs({operand})::text"),
            Answer::Value(value.clone().abs().to_string()),
        ));
        cases.push((
            format!("SELECT floor({operand})::text"),
            Answer::of(value.clone().floor()),
        ));
        cases.push((
            format!("SELECT ceil({operand})::text"),
            Answer::of(value.clone().ceil()),
        ));
        cases.push((
            format!("SELECT round({operand})::text"),
            Answer::of(value.clone().round(0)),
        ));
        cases.push((
            format!("SELECT trunc({operand})::text"),
            Answer::Value(value.clone().trunc(0).to_string()),
        ));
        for &scale in SCALES {
            cases.push((
                format!("SELECT round({operand}, {scale})::text"),
                Answer::of(value.clone().round(scale)),
            ));
            cases.push((
                format!("SELECT trunc({operand}, {scale})::text"),
                Answer::Value(value.clone().trunc(scale).to_string()),
            ));
        }
    }
    compare(&mut conn, cases);
}

/// The predicates agree with PostgreSQL's comparisons of the value with zero and with its own
/// truncation.
#[test]
fn pg18_classifies_every_operand() {
    let mut conn = answers::connect(&PG18, PG18_TAG);
    let mut cases = Vec::new();
    for &text in OPERANDS {
        let (value, operand) = (numeric(text), sql(text));
        let classes = format!(
            "{}|{}|{}|{}|{}",
            value.is_nan(),
            value.is_infinite(),
            value.is_zero(),
            value.is_negative(),
            value.is_integral(),
        );
        cases.push((
            format!(
                "SELECT format('%s|%s|%s|%s|%s', {operand} = 'NaN', \
                 abs({operand}) = 'Infinity', {operand} = 0, {operand} < 0, \
                 {operand} NOT IN ('NaN', 'Infinity', '-Infinity') AND {operand} = trunc({operand}))"
            ),
            Answer::Value(classes.replace("true", "t").replace("false", "f")),
        ));
    }
    compare(&mut conn, cases);
}

/// Non-decimal zeros, and hexadecimal integers either side of the largest weight PostgreSQL
/// stores, refused once at the check after a full group of digits and once after the last one.
#[test]
fn pg18_reads_non_decimal_integers_at_the_weight_bound() {
    let mut conn = answers::connect(&PG18, PG18_TAG);
    let hex = |digits| format!("0x{}", "F".repeat(digits));
    let texts = [
        "0x0".to_owned(),
        "-0o_0".to_owned(),
        "0b0".to_owned(),
        hex(108_852),
        hex(108_853),
        hex(108_856),
    ];
    let cases = texts
        .iter()
        .map(|text| {
            (
                format!("SELECT {}::text", sql(text)),
                Answer::of(PgNumeric::parse(text)),
            )
        })
        .collect();
    compare(&mut conn, cases);
}

/// `int2_numeric`, `int4_numeric` and `int8_numeric` at each type's bounds.
#[test]
fn pg18_converts_every_integer_type() {
    let mut conn = answers::connect(&PG18, PG18_TAG);
    let mut cases = Vec::new();
    for value in [i16::MIN, -1, 0, i16::MAX] {
        cases.push((
            format!("SELECT ({value})::smallint::numeric::text"),
            Answer::Value(PgNumeric::from(value).to_string()),
        ));
    }
    for value in [i32::MIN, i32::MAX] {
        cases.push((
            format!("SELECT ({value})::integer::numeric::text"),
            Answer::Value(PgNumeric::from(value).to_string()),
        ));
    }
    for value in [i64::MIN, i64::MAX] {
        cases.push((
            format!("SELECT ({value})::bigint::numeric::text"),
            Answer::Value(PgNumeric::from(value).to_string()),
        ));
    }
    compare(&mut conn, cases);
}
