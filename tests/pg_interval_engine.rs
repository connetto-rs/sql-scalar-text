//! Intervals read, printed and compared as PostgreSQL 18 does, with PostgreSQL 16 pinning only
//! the printed text since its literal grammar differs (it accepts `1 day ago 2`).
//!
//! PostgreSQL 18 runs on the Debian image because `interval_in` reads ISO 8601 numbers with the
//! C library's `strtod`, and musl reads `PT4.9e-324S` as zero where glibc refuses it.

#[path = "support/containers.rs"]
mod containers;
#[path = "support/exit_slot.rs"]
mod exit_slot;
#[path = "support/interval_literals.rs"]
mod interval_literals;
#[path = "support/postgres.rs"]
mod postgres;

use std::{
    cmp::Ordering,
    hash::{BuildHasher, RandomState},
};

use diesel::{
    dsl::sql,
    prelude::*,
    sql_types::{Bool, Integer, Text},
};
use interval_literals::LITERALS;
use sql_scalar_text::{
    IntervalFields, IntervalTypmod, PgInterval, parse_interval, parse_interval_typmod,
};

static PG18: postgres::Slot = exit_slot::ExitSlot::new();
static PG16: postgres::Slot = exit_slot::ExitSlot::new();

const PG18_TAG: &str = "18.6";
const PG16_TAG: &str = "16.15-alpine";

/// Every interval type PostgreSQL's grammar can write, with the crate's typmod for it.
fn typmods() -> Vec<(&'static str, IntervalTypmod)> {
    use IntervalFields::{
        Day, DayToHour, DayToMinute, DayToSecond, Hour, HourToMinute, HourToSecond, Minute,
        MinuteToSecond, Month, Second, Year, YearToMonth,
    };
    let typmod = |fields, precision| IntervalTypmod::new(fields, precision).unwrap();
    vec![
        ("interval", IntervalTypmod::default()),
        ("interval(0)", typmod(None, Some(0))),
        ("interval(3)", typmod(None, Some(3))),
        ("interval(6)", typmod(None, Some(6))),
        ("interval year", typmod(Some(Year), None)),
        ("interval month", typmod(Some(Month), None)),
        ("interval day", typmod(Some(Day), None)),
        ("interval hour", typmod(Some(Hour), None)),
        ("interval minute", typmod(Some(Minute), None)),
        ("interval second", typmod(Some(Second), None)),
        ("interval second(0)", typmod(Some(Second), Some(0))),
        ("interval second(3)", typmod(Some(Second), Some(3))),
        ("interval second(9)", typmod(Some(Second), Some(9))),
        ("interval year to month", typmod(Some(YearToMonth), None)),
        ("interval day to hour", typmod(Some(DayToHour), None)),
        ("interval day to minute", typmod(Some(DayToMinute), None)),
        ("interval day to second", typmod(Some(DayToSecond), None)),
        (
            "interval day to second(2)",
            typmod(Some(DayToSecond), Some(2)),
        ),
        ("interval hour to minute", typmod(Some(HourToMinute), None)),
        ("interval hour to second", typmod(Some(HourToSecond), None)),
        (
            "interval hour to second(1)",
            typmod(Some(HourToSecond), Some(1)),
        ),
        (
            "interval minute to second",
            typmod(Some(MinuteToSecond), None),
        ),
        (
            "interval minute to second(4)",
            typmod(Some(MinuteToSecond), Some(4)),
        ),
    ]
}

/// The text PostgreSQL prints for `literal` read as `type_name`, or `None` when it refuses it.
fn pg_interval_text(conn: &mut PgConnection, literal: &str, type_name: &str) -> Option<String> {
    // Raw SQL, since `pg_input_is_valid` has no DSL form.
    let valid: bool = diesel::select(
        sql::<Bool>("pg_input_is_valid(")
            .bind::<Text, _>(literal)
            .sql(", ")
            .bind::<Text, _>(type_name)
            .sql(")"),
    )
    .get_result(conn)
    .unwrap_or_else(|e| panic!("pg_input_is_valid({literal:?}, {type_name:?}): {e}"));
    if !valid {
        return None;
    }
    // Raw SQL with the literal inline, since a bound parameter skips the typmod in `interval_in`.
    let quoted = format!("'{}'", literal.replace('\'', "''"));
    let text: String = diesel::select(sql::<Text>(&format!("({quoted}::{type_name})::text")))
        .get_result(conn)
        .unwrap_or_else(|e| panic!("{literal:?}::{type_name}: {e}"));
    Some(text)
}

/// PostgreSQL's `interval_cmp` of two literals it accepts.
fn pg_interval_cmp(conn: &mut PgConnection, left: &str, right: &str) -> Ordering {
    // Raw SQL, since `interval_cmp` is a btree support function the DSL does not name.
    let cmp: i32 = diesel::select(
        sql::<Integer>("interval_cmp(")
            .bind::<Text, _>(left)
            .sql("::interval, ")
            .bind::<Text, _>(right)
            .sql("::interval)"),
    )
    .get_result(conn)
    .unwrap_or_else(|e| panic!("interval_cmp({left:?}, {right:?}): {e}"));
    cmp.cmp(&0)
}

/// Field values at every unit carry, sign change and sentinel extreme.
fn printer_fields() -> Vec<PgInterval> {
    let months = [0, 1, -1, 11, 12, -13, 25, i32::MAX, i32::MIN];
    let days = [0, 1, -1, 30, -31, i32::MAX, i32::MIN];
    let microseconds = [
        0,
        1,
        -1,
        500,
        -1_000_000,
        59_999_999,
        3_600_000_000,
        -86_399_999_999,
        86_400_000_000,
        123_456_789_012,
        i64::MAX,
        i64::MIN,
    ];
    let mut fields = Vec::new();
    for &months in &months {
        for &days in &days {
            for &microseconds in &microseconds {
                fields.push(PgInterval {
                    months,
                    days,
                    microseconds,
                });
            }
        }
    }
    fields
}

/// The literal PostgreSQL reads back as exactly these fields.
fn fields_literal(interval: &PgInterval) -> String {
    format!(
        "{} mons {} days {} microseconds",
        interval.months, interval.days, interval.microseconds
    )
}

/// The printed text of every boundary field value, and that text read back, against the engine.
fn assert_printer_matches(conn: &mut PgConnection, holds_infinity: bool) {
    let mut mismatches = Vec::new();
    for interval in printer_fields() {
        if !holds_infinity && !interval.is_finite() {
            continue;
        }
        let literal = fields_literal(&interval);
        let expected = pg_interval_text(conn, &literal, "interval");
        let printed = interval.to_string();
        if expected.as_deref() != Some(printed.as_str()) {
            mismatches.push(format!(
                "{interval:?}: crate {printed:?}, PostgreSQL {expected:?}"
            ));
        }
        let reread = parse_interval(&printed);
        let engine_reread = pg_interval_text(conn, &printed, "interval");
        if reread.map(|i| i.to_string()) != engine_reread {
            mismatches.push(format!(
                "{printed:?} reads back as {reread:?}, PostgreSQL {engine_reread:?}"
            ));
        }
        if let Some(reread) = reread
            && (reread.months, reread.days, reread.microseconds)
                != (interval.months, interval.days, interval.microseconds)
        {
            mismatches.push(format!(
                "{interval:?}: {printed:?} reads back as {reread:?}"
            ));
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

/// Every literal of the corpus under every interval type.
#[test]
fn pg18_reads_every_literal_under_every_interval_type() {
    let mut conn = postgres::connect(&PG18, PG18_TAG);
    let mut mismatches = Vec::new();
    for (type_name, typmod) in typmods() {
        for &literal in LITERALS {
            let got = parse_interval_typmod(literal, typmod).map(|i| i.to_string());
            let expected = pg_interval_text(&mut conn, literal, type_name);
            if got != expected {
                mismatches.push(format!(
                    "{literal:?}::{type_name}: crate {got:?}, PostgreSQL {expected:?}"
                ));
            }
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

#[test]
fn pg18_prints_every_boundary_of_the_fields() {
    let mut conn = postgres::connect(&PG18, PG18_TAG);
    assert_printer_matches(&mut conn, true);
}

#[test]
fn pg16_prints_every_finite_boundary_of_the_fields() {
    let mut conn = postgres::connect(&PG16, PG16_TAG);
    assert_printer_matches(&mut conn, false);
}

/// Every pair of accepted values orders as `interval_cmp` orders it, and equal pairs hash equal.
#[test]
fn pg18_orders_and_equates_every_pair() {
    let mut conn = postgres::connect(&PG18, PG18_TAG);
    let mut values: Vec<(String, PgInterval)> = LITERALS
        .iter()
        .filter_map(|&literal| Some((literal.to_owned(), parse_interval(literal)?)))
        .collect();
    values.extend(
        printer_fields()
            .into_iter()
            .step_by(7)
            .map(|interval| (fields_literal(&interval), interval)),
    );
    let hasher = RandomState::new();
    let mut mismatches = Vec::new();
    for (index, (left_text, left)) in values.iter().enumerate() {
        for (right_text, right) in &values[index..] {
            let expected = pg_interval_cmp(&mut conn, left_text, right_text);
            if left.cmp(right) != expected {
                mismatches.push(format!(
                    "{left_text:?} vs {right_text:?}: crate {:?}, PostgreSQL {expected:?}",
                    left.cmp(right)
                ));
            }
            if expected == Ordering::Equal && hasher.hash_one(left) != hasher.hash_one(right) {
                mismatches.push(format!(
                    "{left_text:?} and {right_text:?} are equal but hash apart"
                ));
            }
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}
