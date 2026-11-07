//! Explicit casts of literals answered as PostgreSQL 18 answers them, each value by the text its
//! output function prints and each refusal by the message PostgreSQL raises.

#[path = "support/containers.rs"]
mod containers;
#[path = "support/exit_slot.rs"]
mod exit_slot;
#[path = "support/postgres.rs"]
mod postgres;

use std::{
    cmp::Ordering,
    hash::{BuildHasher, RandomState},
};

use diesel::{
    dsl::sql,
    prelude::*,
    result::Error as DieselError,
    sql_types::{Bool, Integer, Text},
};
use sql_values::{
    CastError, CastTarget, CastValue, CharLength, Literal, NumericTypmod, PgNumeric, PgType,
    cast_literal,
};

static PG18: postgres::Slot = exit_slot::ExitSlot::new();

const PG18_TAG: &str = "18.6";

define_sql_function!(fn set_config(setting: Text, value: Text, is_local: Bool) -> Text);

/// Every target with the SQL that names it.
fn targets() -> Vec<(&'static str, CastTarget)> {
    let length = |n| Some(CharLength::new(n).expect("a valid length"));
    let numeric = |p, s| CastTarget::Numeric(Some(NumericTypmod::new(p, s).expect("a typmod")));
    vec![
        ("boolean", CastTarget::Boolean),
        ("smallint", CastTarget::Smallint),
        ("integer", CastTarget::Integer),
        ("bigint", CastTarget::Bigint),
        ("numeric", CastTarget::Numeric(None)),
        ("numeric(4,1)", numeric(4, 1)),
        ("numeric(3)", numeric(3, 0)),
        ("numeric(2,-1)", numeric(2, -1)),
        ("numeric(3,3)", numeric(3, 3)),
        ("numeric(1000,500)", numeric(1000, 500)),
        ("text", CastTarget::Text),
        ("varchar", CastTarget::Varchar(None)),
        ("varchar(1)", CastTarget::Varchar(length(1))),
        ("varchar(3)", CastTarget::Varchar(length(3))),
        ("char", CastTarget::Bpchar(length(1))),
        ("char(4)", CastTarget::Bpchar(length(4))),
        ("bpchar", CastTarget::Bpchar(None)),
        ("date", CastTarget::Date),
        ("timestamp", CastTarget::Timestamp(None)),
        ("timestamp(0)", CastTarget::Timestamp(Some(0))),
        ("timestamp(3)", CastTarget::Timestamp(Some(3))),
        ("timestamp(6)", CastTarget::Timestamp(Some(6))),
        ("timestamptz", CastTarget::TimestampTz(None)),
        ("timestamptz(0)", CastTarget::TimestampTz(Some(0))),
    ]
}

/// String constants reaching every input function's branches.
const QUOTED: &[&str] = &[
    // boolean
    "t",
    "TRUE",
    "tr",
    "True ",
    "  yes",
    "y",
    "Ye",
    "no",
    "N",
    "on",
    "ON",
    "of",
    "Off",
    "o",
    "onn",
    "ofx",
    "truex",
    "1",
    "0",
    "10",
    "",
    "  ",
    "\x0bf\x0c",
    // integers
    "-0",
    "+12",
    " 12 ",
    "\t12\n",
    "\x0b12",
    "2.5",
    "1e2",
    "0x1F",
    "-0x80000000",
    "0X7fff",
    "0o17",
    "0b101",
    "0b_1",
    "0x_1F",
    "1_000",
    "_1",
    "1_",
    "1__0",
    "0x",
    "0x1_",
    "-0x",
    "32767",
    "32768",
    "-32768",
    "-32769",
    "2147483647",
    "2147483648",
    "-2147483648",
    "-2147483649",
    "2147483649x",
    "21474836480x",
    "9223372036854775807",
    "9223372036854775808",
    "-9223372036854775808",
    "-9223372036854775809",
    "99999999999999999999x",
    "0x8000000000000000",
    "-0x8000000000000000",
    "abc",
    "- 5",
    "+-5",
    "１２",
    // numeric
    "1.50",
    ".5",
    "5.",
    "-0.0",
    "1e-3",
    "1.5E+3",
    "1_000.5",
    "1_000_.5",
    "1._5",
    "1e1_0",
    "1e_1",
    "1e",
    ".e5",
    "NaN",
    "nan",
    "-NaN",
    "+NaN",
    "Infinity",
    "-inf",
    "+INF",
    "infinity ",
    "infx",
    "Infinityx",
    "2.7",
    "-2.7",
    "12.345",
    "99.95",
    "123.45",
    "0.0005",
    "-0.0004",
    "0.05",
    "15",
    "95",
    "1e1000",
    "-1e-500",
    "1e131072",
    "1e-16383",
    "1e-16384",
    "0e-16384",
    "1e1073741823",
    "1e1073741824x",
    "0x1F.5",
    "0xFFFFFFFFFFFFFFFFFFFF",
    "-0b1111",
    "1e2x",
    "0.00049",
    "0.5",
    "-0.5",
    "9223372036854775807.5",
    "-9223372036854775808.5",
    // character
    "abc",
    "abc  ",
    "ab",
    "héllo",
    "日本語テキスト",
    "  lead",
    "a'b",
    // date and time
    "2026-01-01",
    "2026-01-01 23:00",
    "2026-01-01T23:00:00+05",
    "2026-01-01 23:00:00.6",
    "2026-01-01 23:00:00.5",
    "2026-01-01 23:00:00.4999995",
    "2026-01-01 23:00:00.123456",
    "2026-01-01 23:00+0530",
    "2026-01-01 23:00-15:59:59",
    "2026-01-01 23:00+16",
    "2026-01-01 23:00Z",
    "2026-01-01 BC",
    "2026-01-01 23:00:00.5 BC",
    "0001-01-01 BC",
    "0000-01-01",
    "0000-01-01 BC",
    "2026-13-01",
    "2026-00-01",
    "2026-01-00",
    "2026-02-29",
    "2024-02-29",
    "2026-04-31",
    "0001-02-29 BC",
    "4714-11-24 BC",
    "4714-11-23 BC",
    "4714-11-23 23:00-02 BC",
    "4714-11-24 00:00+01 BC",
    "4714-10-31 23:00-02 BC",
    "4714-11-24 00:00:00.4 BC",
    "4714-11-24 00:00:00.6 BC",
    "4715-01-01 BC",
    "300000-01-01 BC",
    "100000-01-01",
    "262142-12-31 23:59:59.999999",
    " 2026-01-01 ",
    "\t2026-01-01\n",
    "2026-01-01 24:00",
    "2026-01-01 23:60",
    "2026-01-01 23:59:60",
    "today",
    "now",
    "epoch",
    "infinity",
    "01/02/2026",
    "2026-1-1",
    "1999-12-31 23:59:59.5",
    "2000-01-01 00:00:00.5",
    "1999-12-31 23:59:59.4999",
    "2030-01-01 00:00:00+02",
    "2030-01-01 00:00:00",
    "2030-01-01 00:00:00.5+00:00:01",
    "5874897-12-31",
    "5874898-01-01",
    "5874897-12-31 BC",
    "294276-12-31 23:59:59.999999",
    "294276-12-31 23:59:59.9999995",
    "294277-01-01 00:59:59+01",
    "294276-12-31 23:59:59.6",
    "294277-01-01",
    "4714-11-23 23:59:59.999999 BC",
    "12345678-01-01",
    "1000000000-01-01",
    "2147483648-01-01",
    "+infinity",
    "-infinity",
    " -INFINITY ",
    "Infinity",
    "EPOCH",
    "2026-01-01 24:00:01",
    "2026-01-01 24:00:00.5",
    "2026-01-01 23:59:60.5",
    "2026-01-01 25:00+16",
    "2026-01-01 23:61+16",
    "2026-13-01 12:00+16",
    "2026-01-01 12:00:00.1234565",
    "2026-01-01 12:00:00.1234575",
    "2026-01-01 12:00:00.",
    "2026-01-01  12:00",
    "2026-01-01t12:00",
    "2026-01-01 12:00:00z",
    "2026-01-01 12:00:00 Z",
    "2026-01-01 12:00:00 +05",
    "2026-01-01 12:00+5",
    "2026-01-01 12:00+123",
    "2026-01-01 12:00+05:3",
    "2026-01-01 12:00+0560",
    "2026-01-01 12:00+05:30:60",
    "2026-01-01 12:00-00:00:01",
    "2026-01-01 12:00:00 bc",
    "2026-01-01 12:00:00BC",
    "2026-01-01BC",
    "2026-01-01 AD",
    "2026-01-01 12:00 AD",
    "2026-01-01 12:00:00 BC AD",
    "10000-01-01",
    "2026-01-01 12:00:00Z BC",
    "2026-01-01 12:00 CEST",
    "2026-01-01 12:00CEST",
    "2026-01-01 12:00 cest bc",
    "2026-01-01 12:00 BC CEST",
    "2026-01-01 12:00 CEST CET",
    "2026-01-01 12:00+01 CET",
    "2026-01-01 MSK",
    "2010-07-15 12:00 MSK",
    "2012-01-15 12:00 msk",
    "2026-01-01 12:00 zulu",
    "2026-01-01 12:00 UTC",
    "2026-01-01 12:00 factory",
    "2026-01-01 12:00 nowhere",
    "4714-11-24 00:30 BC CET",
    "294276-12-31 23:30 CET",
    "294277-01-01 00:30 CET",
];

/// Numeric constants of every lexer token and every type `make_const` gives them.
const NUMBERS: &[&str] = &[
    "0",
    "-0",
    "2",
    "2.5",
    "-2.5",
    "2.7",
    "-2.7",
    "1e2",
    "1.5e-3",
    "1.50",
    ".5",
    "5.",
    "1_000",
    "0x1F",
    "-0x1F",
    "0o17",
    "0b101",
    "0x_1F",
    "32767",
    "32768",
    "-32768",
    "-32769",
    "2147483647",
    "2147483648",
    "-2147483648",
    "-2147483649",
    "9223372036854775807",
    "9223372036854775808",
    "-9223372036854775808",
    "-9223372036854775809",
    "9223372036854775807.4",
    "9223372036854775807.5",
    "-9223372036854775808.5",
    "0xFFFFFFFFFFFFFFFFF",
    "-0x8000000000000000",
    "1e20",
    "12.345",
    "99.95",
    "0.0005",
    "-0.0004",
    "12345",
    "1e-16383",
    "1e-16384",
    "1e1073741824",
    "1.e5",
    "0.5",
    "-0.5",
    "1__0",
    "0x",
    "1e",
    "abc",
    "--1",
];

/// The crate's error for a message PostgreSQL raises, which names the input in quotes.
fn pg_error(message: &str) -> CastError {
    let stem = match message.strip_prefix("value \"") {
        Some(rest) => {
            let (_, type_name) = rest
                .split_once("\" is out of range for type ")
                .unwrap_or_else(|| panic!("PostgreSQL raised {message:?}"));
            format!("value is out of range for type {type_name}")
        }
        None => message
            .split_once(": \"")
            .map_or(message, |(stem, _)| stem)
            .to_owned(),
    };
    let types = [
        PgType::Boolean,
        PgType::Smallint,
        PgType::Integer,
        PgType::Bigint,
        PgType::Numeric,
        PgType::Text,
        PgType::Varchar,
        PgType::Bpchar,
        PgType::Date,
        PgType::Timestamp,
        PgType::TimestampTz,
    ];
    let mut errors = vec![
        CastError::NumericFieldOverflow,
        CastError::NumericFormatOverflow,
        CastError::DateTimeFieldOutOfRange,
        CastError::DateOutOfRange,
        CastError::TimestampOutOfRange,
        CastError::TimeZoneDisplacementOutOfRange,
    ];
    for pg_type in types {
        errors.push(CastError::InvalidInputSyntax(pg_type));
        errors.push(CastError::ValueOutOfRange(pg_type));
        errors.push(CastError::OutOfRange(pg_type));
        for from in [PgType::Integer, PgType::Bigint, PgType::Numeric] {
            errors.push(CastError::CannotCast { from, to: pg_type });
        }
    }
    errors
        .into_iter()
        .find(|error| error.to_string() == stem)
        .unwrap_or_else(|| panic!("PostgreSQL raised {message:?}"))
}

/// The text `expression` prints through its type's output function, or the error it raises.
fn pg_text(conn: &mut PgConnection, expression: &str) -> Result<String, CastError> {
    // Raw SQL with the literal inline, since a bound parameter is typed `text` and casts as
    // `text` does, never as the unknown literal under test.
    match diesel::select(sql::<Text>(&format!("format('%s', {expression})"))).get_result(conn) {
        Ok(text) => Ok(text),
        Err(DieselError::DatabaseError(_, info)) => Err(pg_error(info.message())),
        Err(other) => panic!("{expression}: {other}"),
    }
}

fn connect() -> PgConnection {
    let mut conn = postgres::connect(&PG18, PG18_TAG);
    let _: String = diesel::select(set_config("TimeZone", "UTC", false))
        .get_result(&mut conn)
        .expect("set TimeZone");
    conn
}

fn quoted(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// Every literal under every target, skipping the casts the crate declines.
#[test]
fn pg18_casts_every_literal_to_every_target() {
    let mut conn = connect();
    let mut mismatches = Vec::new();
    let mut answered = 0;
    let literals = QUOTED
        .iter()
        .map(|&text| (Literal::Quoted(text), quoted(text)))
        .chain(
            NUMBERS
                .iter()
                .map(|&text| (Literal::Number(text), format!("({text})"))),
        );
    for (literal, sql_literal) in literals {
        for (type_name, target) in targets() {
            let got = match cast_literal(literal, target) {
                Err(CastError::Unsupported) => continue,
                got => got.map(|value| value.to_string()),
            };
            answered += 1;
            let expression = format!("{sql_literal}::{type_name}");
            let expected = pg_text(&mut conn, &expression);
            if got != expected {
                mismatches.push(format!(
                    "{expression}: crate {got:?}, PostgreSQL {expected:?}"
                ));
            }
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
    assert!(answered > 2_000, "only {answered} casts answered");
}

fn numeric(text: &str) -> PgNumeric {
    match cast_literal(Literal::Quoted(text), CastTarget::Numeric(None)) {
        Ok(CastValue::Numeric(value)) => value,
        other => panic!("{text:?}::numeric: {other:?}"),
    }
}

/// PostgreSQL's `numeric_cmp` of two literals it reads.
fn pg_numeric_cmp(conn: &mut PgConnection, left: &str, right: &str) -> Ordering {
    // Raw SQL, since diesel has no typed cast from `text` to `numeric`.
    let cmp: i32 = diesel::select(
        sql::<Integer>("numeric_cmp(")
            .bind::<Text, _>(left)
            .sql("::numeric, ")
            .bind::<Text, _>(right)
            .sql("::numeric)"),
    )
    .get_result(conn)
    .unwrap_or_else(|e| panic!("numeric_cmp({left:?}, {right:?}): {e}"));
    cmp.cmp(&0)
}

/// Every pair of numerics, in both orders, orders as `numeric_cmp` orders it, and equal pairs hash
/// equal.
#[test]
fn pg18_orders_and_equates_every_numeric_pair() {
    let mut conn = connect();
    let texts = [
        "NaN",
        "Infinity",
        "-Infinity",
        "0",
        "-0.00",
        "0.000",
        "1",
        "1.0",
        "1.50",
        "1.5",
        "-1.5",
        "-1.50",
        "10",
        "1e1",
        "9.99",
        "-9.99",
        "-10",
        "0.001",
        "-0.001",
        "1e100",
        "-1e100",
        "123456789012345678901234567890",
        "123456789012345678901234567891",
        "0.1",
        "0.10000001",
    ];
    let hasher = RandomState::new();
    let mut mismatches = Vec::new();
    for left_text in &texts {
        for right_text in &texts {
            let (left, right) = (numeric(left_text), numeric(right_text));
            let expected = pg_numeric_cmp(&mut conn, left_text, right_text);
            if left.cmp(&right) != expected {
                mismatches.push(format!(
                    "{left_text:?} vs {right_text:?}: crate {:?}, PostgreSQL {expected:?}",
                    left.cmp(&right)
                ));
            }
            if expected == Ordering::Equal && hasher.hash_one(&left) != hasher.hash_one(&right) {
                mismatches.push(format!(
                    "{left_text:?} and {right_text:?} are equal but hash apart"
                ));
            }
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}
