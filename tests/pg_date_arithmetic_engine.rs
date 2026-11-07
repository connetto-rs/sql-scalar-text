//! `date` arithmetic, the cast to `timestamp` and the comparison with `timestamp` computed as
//! PostgreSQL 18 computes them, every answer and every refusal's message measured on the engine.

#[path = "support/answers.rs"]
mod answers;
#[path = "support/containers.rs"]
mod containers;
#[path = "support/exit_slot.rs"]
mod exit_slot;
#[path = "support/postgres.rs"]
mod postgres;

use answers::{Answer, compare, quoted};
use sql_values::{CastTarget, CastValue, InputContext, Literal, PgDate, PgTimestamp, cast_literal};

static PG18: postgres::Slot = exit_slot::ExitSlot::new();

const PG18_TAG: &str = "18.6";

/// Dates at both ends of PostgreSQL's range and of the timestamp range, month ends, a leap day,
/// the epoch and both infinities.
const DATES: &[&str] = &[
    "2026-01-31",
    "2026-03-01",
    "2024-02-29",
    "2000-01-01",
    "0001-01-01",
    "4713-01-01 BC",
    "4714-11-24 BC",
    "4714-11-25 BC",
    "294276-12-31",
    "294277-01-01",
    "5874897-12-30",
    "5874897-12-31",
    "infinity",
    "-infinity",
];

/// Day counts at each overflow and range boundary and around zero.
const DAYS: &[i32] = &[
    0,
    1,
    -1,
    31,
    -31,
    365,
    2_451_545,
    -2_451_545,
    2_145_031_948,
    -2_145_031_948,
    i32::MAX,
    i32::MIN,
];

/// Timestamps around the dates above, the stored 294277-01-01 that `timestamp(0)` rounds the
/// last second up to, and both infinities, each with the SQL that makes it.
const TIMESTAMPS: &[(&str, &str)] = &[
    ("2026-01-31 00:00:00", "timestamp"),
    ("2026-01-31 10:00:00", "timestamp"),
    ("2026-01-30 23:59:59.999999", "timestamp"),
    ("2000-01-01 00:00:00", "timestamp"),
    ("4714-11-24 00:00:00 BC", "timestamp"),
    ("294276-12-31 00:00:00", "timestamp"),
    ("294276-12-31 23:59:59.999999", "timestamp"),
    ("294276-12-31 23:59:59.5", "timestamp(0)"),
    ("infinity", "timestamp"),
    ("-infinity", "timestamp"),
];

fn date(text: &str) -> PgDate {
    PgDate::parse(text, InputContext::new()).unwrap_or_else(|error| panic!("{text:?}: {error}"))
}

fn timestamp(text: &str, type_name: &str) -> PgTimestamp {
    let target = match type_name {
        "timestamp" => CastTarget::Timestamp(None),
        _ => CastTarget::Timestamp(Some(0)),
    };
    match cast_literal(Literal::Quoted(text), target) {
        Ok(CastValue::Timestamp(at)) => at,
        other => panic!("{text:?}::{type_name}: {other:?}"),
    }
}

#[test]
fn pg18_shifts_every_date_by_every_day_count() {
    let mut conn = answers::connect(&PG18, PG18_TAG);
    let mut cases = Vec::new();
    for &text in DATES {
        let (value, operand) = (date(text), format!("{}::date", quoted(text)));
        for &days in DAYS {
            for (operator, result) in [
                ("+", value.checked_add_days(days)),
                ("-", value.checked_sub_days(days)),
            ] {
                cases.push((
                    format!("SELECT ({operand} {operator} ({days})::integer)::text"),
                    Answer::of(result),
                ));
            }
            cases.push((
                format!("SELECT (({days})::integer + {operand})::text"),
                Answer::of(value.checked_add_days(days)),
            ));
        }
        for &earlier in DATES {
            cases.push((
                format!("SELECT ({operand} - {}::date)::text", quoted(earlier)),
                Answer::of(value.days_since(date(earlier))),
            ));
        }
        cases.push((
            format!("SELECT ({operand}::timestamp)::text"),
            Answer::of(value.to_timestamp()),
        ));
    }
    compare(&mut conn, cases);
}

#[test]
fn pg18_orders_every_date_against_every_timestamp() {
    let mut conn = answers::connect(&PG18, PG18_TAG);
    let mut cases = Vec::new();
    for &text in DATES {
        let (value, operand) = (date(text), format!("{}::date", quoted(text)));
        for &(at_text, type_name) in TIMESTAMPS {
            let at = timestamp(at_text, type_name);
            let other = format!("{}::{type_name}", quoted(at_text));
            let order = value.cmp_timestamp(at) as i8;
            cases.push((
                format!(
                    "SELECT CASE WHEN {operand} < {other} THEN -1 \
                     WHEN {operand} = {other} THEN 0 ELSE 1 END::text"
                ),
                Answer::Value(order.to_string()),
            ));
        }
    }
    compare(&mut conn, cases);
}
