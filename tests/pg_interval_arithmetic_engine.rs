//! Interval arithmetic computed as PostgreSQL 18 computes it, every answer and every refusal's
//! message measured on the engine.

#[path = "support/containers.rs"]
mod containers;
#[path = "support/exit_slot.rs"]
mod exit_slot;
#[path = "support/postgres.rs"]
mod postgres;

use std::fmt::Display;

use diesel::{
    dsl::sql,
    prelude::*,
    sql_types::{Array, Text},
};
use sql_scalar_text::{
    IntervalSum, PgInterval, ShiftError, parse_f64, parse_interval, parse_time, parse_timestamp,
};

static PG18: postgres::Slot = exit_slot::ExitSlot::new();

const PG18_TAG: &str = "18.6";

/// What PostgreSQL answers for a query of one text value, `None` for SQL `NULL`.
type Answer = Result<Option<String>, String>;

/// Intervals at every infinity, field bound and sign mix, as PostgreSQL prints them.
const OPERANDS: &[&str] = &[
    "infinity",
    "-infinity",
    "00:00:00",
    "1 day",
    "-1 days",
    "1 mon",
    "-1 mons",
    "1 year 2 mons 3 days 04:05:06.789",
    "1 mon -1 days +01:00:00",
    "-1 years -9 mons",
    "-2 years +3 mons -00:00:00.5",
    "00:00:00.000001",
    "23:59:59.999999",
    "36:00:00",
    "-1 days -12:00:00",
    "12 mons 30 days",
    "1 mon 15 days",
    "7 days 12:00:00",
    "-1 mons +29 days -23:59:59",
    "1 mon 1 day 00:00:00.000001",
    "-3 mons -7 days -00:00:00.000001",
    "100 years 100 days 100:00:00",
    "10000 years",
    "-2025 years",
    "2147483647 days",
    "-2147483648 days",
    "178956970 years 7 mons",
    "-178956970 years -8 mons",
    "2562047788:00:54.775807",
    "178956970 years 7 mons 2147483647 days 2562047788:00:54.775806",
    "-178956970 years -8 mons -2147483648 days -2562047788:00:54.775807",
    "70474776 mons",
    "2145031949 days",
    "3 mons -29 days +2562047788:00:54.775807",
    "1 year -1 days -01:00:00",
    "-14283278 years -6 mons +539360863 days",
    "40967461 years 5 mons -198916822 days -51639315:05:16.989153",
    "-1 years +2147483647 days -2562047788:00:54.775806",
    "2 years -2147483648 days +00:00:00.99",
    "-1 years -1 mons +2562047788:00:54.775807",
    "178956970 years 7 mons -00:00:00.000001",
    "51764879 years 10 mons 543425336 days 19259456:58:56.752156",
    "-2 years -11 mons -2147483648 days -01:00:00",
    "-83858867 years -9 mons -462097977 days -52830097:47:29.797211",
    "-178956970 years -8 mons +01:00:00",
];

/// Factors at every sign, scale, fraction that cascades, and special value.
const FACTORS: &[&str] = &[
    "0",
    "-0",
    "1",
    "-1",
    "2",
    "-3",
    "7",
    "12",
    "30",
    "1000000",
    "2147483647",
    "-2147483648",
    "1.5",
    "0.1",
    "-2.5",
    "0.33333333333333333333",
    "0.5",
    "1.000001",
    "3.14159",
    "1e-7",
    "1e9",
    "0.0001",
    "86400.5",
    "-0.75",
    "2.5e-10",
    "1e300",
    "1e-300",
    "Infinity",
    "-Infinity",
    "NaN",
];

/// Timestamps at month ends, leap days, the epoch, the first PostgreSQL holds and the last year
/// `chrono` holds.
const TIMESTAMPS: &[&str] = &[
    "2026-01-31 10:20:30.5",
    "2024-02-29 23:59:59.999",
    "2024-03-31 00:00:00",
    "2000-01-01 00:00:00",
    "1999-12-31 23:59:59.999999",
    "0001-01-01 00:00:00",
    "0001-12-31 00:00:00 BC",
    "4714-11-24 00:00:00 BC",
    "4714-11-24 00:00:00.000001 BC",
    "4714-11-24 12:00:00 BC",
    "9999-12-31 23:59:59",
    "262000-06-30 12:00:00",
    "262142-12-31 23:59:59.999999",
];

const TIMES: &[&str] = &[
    "00:00:00",
    "10:20:30.5",
    "12:00:00.000001",
    "23:59:59.999999",
];

/// Rows summed in this order, which PostgreSQL never leaves a field's range for, so its answer is
/// the net total.
const GROUPS: &[&[&str]] = &[
    &[],
    &["1 mon", "2 days", "-01:00:00"],
    &["1 day", "2 days"],
    &["1 mon", "1 mon", "1 mon"],
    &["00:00:00.000001", "00:00:00.000002", "00:00:00.000002"],
    &["infinity", "1 day"],
    &["-infinity", "-infinity", "5 days"],
    &["infinity", "-infinity"],
    &["2147483647 days", "1 day"],
    &["2147483647 days", "-1 days", "1 day"],
    &["178956970 years 7 mons", "-1 mons", "1 day"],
    &[
        "-325 years +846 days 00:00:00.00009",
        "-237 years -3 mons +2602 days 00:00:00.000001",
        "315 years 4 mons -2079 days -00:00:00.000001",
        "152 years 4 mons -1857 days -00:00:00.690878",
        "118 years 8 mons -2504 days -00:00:00.000001",
        "32 years 4 mons 2999 days 00:00:00.00009",
        "-193 years -6 mons +1858 days",
        "-319 years -6 mons +300 days -00:00:00.000001",
        "-192 years -10 mons -295 days -00:00:00.999999",
        "74 years 1 mon -981 days -00:00:00.000001",
        "-49 years -2 mons +1791 days 00:00:00.389832",
        "317 years 9 mons -2578 days +00:00:00.000001",
    ],
];

/// A connection whose `pg_temp.answer` runs one query and answers its text, or `ERROR: ` and
/// the message it raises.
fn connect() -> PgConnection {
    let mut conn = postgres::connect(&PG18, PG18_TAG);
    // Raw SQL, since a function catching each query's error has no DSL form.
    diesel::sql_query(
        "CREATE FUNCTION pg_temp.answer(query text) RETURNS text LANGUAGE plpgsql AS $$
         DECLARE result text;
         BEGIN
             EXECUTE query INTO result;
             RETURN coalesce('VALUE: ' || result, 'NULL');
         EXCEPTION WHEN OTHERS THEN
             RETURN 'ERROR: ' || SQLERRM;
         END $$",
    )
    .execute(&mut conn)
    .expect("create pg_temp.answer");
    conn
}

/// PostgreSQL's answer to every query, in one round trip.
fn answers(conn: &mut PgConnection, queries: &[String]) -> Vec<Answer> {
    // Raw SQL, since `unnest ... WITH ORDINALITY` and a `pg_temp` function have no DSL form.
    let texts: Vec<String> = diesel::select(
        sql::<Array<Text>>("ARRAY(SELECT pg_temp.answer(q) FROM unnest(")
            .bind::<Array<Text>, _>(queries)
            .sql(") WITH ORDINALITY AS t(q, n) ORDER BY n)"),
    )
    .get_result(conn)
    .expect("answer the queries");
    texts
        .into_iter()
        .map(|text| {
            if let Some(value) = text.strip_prefix("VALUE: ") {
                Ok(Some(value.to_owned()))
            } else if let Some(message) = text.strip_prefix("ERROR: ") {
                Err(message.to_owned())
            } else {
                Ok(None)
            }
        })
        .collect()
}

/// `text` quoted as an SQL string literal.
fn quoted(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

fn interval(text: &str) -> PgInterval {
    parse_interval(text).unwrap_or_else(|error| panic!("{text:?}: {error}"))
}

/// The crate's answer in PostgreSQL's words, its value printed or its error's message.
fn worded<T: Display, E: Display>(result: Result<T, E>) -> Answer {
    result
        .map(|value| Some(value.to_string()))
        .map_err(|error| error.to_string())
}

/// PostgreSQL's answer as it reads, for values compared by their printed text.
fn as_printed(answer: Answer) -> Answer {
    answer
}

/// Every query against the crate's answer for it, PostgreSQL's answer read through `read`,
/// listing the disagreements.
fn compare(conn: &mut PgConnection, cases: Vec<(String, Answer)>, read: fn(Answer) -> Answer) {
    let (queries, got): (Vec<String>, Vec<Answer>) = cases.into_iter().unzip();
    let expected = answers(conn, &queries).into_iter().map(read);
    let mismatches: Vec<String> = queries
        .iter()
        .zip(got)
        .zip(expected)
        .filter(|((_, got), expected)| got != expected)
        .map(|((query, got), expected)| format!("{query}: crate {got:?}, PostgreSQL {expected:?}"))
        .collect();
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

/// A timestamp result in PostgreSQL's words, where one `chrono` cannot hold is the text
/// PostgreSQL prints for it and so is compared through [`chrono_answer`].
fn timestamp_answer(result: Result<chrono::NaiveDateTime, ShiftError>) -> Answer {
    match result {
        Ok(at) => Ok(Some(at.format("%Y-%m-%d %H:%M:%S%.f").to_string())),
        Err(ShiftError::Unrepresentable) => Ok(Some("beyond chrono".to_owned())),
        Err(error) => Err(error.to_string()),
    }
}

/// PostgreSQL's timestamp answer in the words of [`timestamp_answer`].
fn chrono_answer(answer: Answer) -> Answer {
    answer.map(|text| {
        text.map(|text| match parse_timestamp(&text) {
            Some(at) => at.format("%Y-%m-%d %H:%M:%S%.f").to_string(),
            None => "beyond chrono".to_owned(),
        })
    })
}

/// PostgreSQL's time answer printed as `chrono` prints the time it reads as.
fn time_answer(answer: Answer) -> Answer {
    answer.map(|text| {
        text.map(|text| {
            parse_time(&text)
                .unwrap_or_else(|| panic!("PostgreSQL printed time {text:?}"))
                .to_string()
        })
    })
}

#[test]
fn pg18_adds_subtracts_and_negates_every_pair() {
    let mut conn = connect();
    let mut cases = Vec::new();
    for &left in OPERANDS {
        cases.push((
            format!("SELECT (-{}::interval)::text", quoted(left)),
            worded(interval(left).checked_neg()),
        ));
        for &right in OPERANDS {
            for (operator, result) in [
                ("+", interval(left).checked_add(interval(right))),
                ("-", interval(left).checked_sub(interval(right))),
            ] {
                cases.push((
                    format!(
                        "SELECT ({}::interval {operator} {}::interval)::text",
                        quoted(left),
                        quoted(right)
                    ),
                    worded(result),
                ));
            }
        }
    }
    compare(&mut conn, cases, as_printed);
}

#[test]
fn pg18_multiplies_and_divides_by_every_factor() {
    let mut conn = connect();
    let mut cases = Vec::new();
    for &span in OPERANDS {
        for &factor in FACTORS {
            let value = parse_f64(factor).unwrap_or_else(|| panic!("{factor:?}"));
            for (operator, result) in [
                ("*", interval(span).checked_mul(value)),
                ("/", interval(span).checked_div(value)),
            ] {
                cases.push((
                    format!(
                        "SELECT ({}::interval {operator} {}::float8)::text",
                        quoted(span),
                        quoted(factor)
                    ),
                    worded(result),
                ));
            }
        }
    }
    compare(&mut conn, cases, as_printed);
}

#[test]
fn pg18_shifts_every_timestamp() {
    let mut conn = connect();
    let mut cases = Vec::new();
    for &at in TIMESTAMPS {
        let timestamp = parse_timestamp(at).unwrap_or_else(|| panic!("{at:?}"));
        for &span in OPERANDS {
            for (operator, result) in [
                ("+", interval(span).add_to_timestamp(timestamp)),
                ("-", interval(span).subtract_from_timestamp(timestamp)),
            ] {
                cases.push((
                    format!(
                        "SELECT ({}::timestamp {operator} {}::interval)::text",
                        quoted(at),
                        quoted(span)
                    ),
                    timestamp_answer(result),
                ));
            }
        }
    }
    compare(&mut conn, cases, chrono_answer);
}

#[test]
fn pg18_subtracts_every_pair_of_timestamps() {
    let mut conn = connect();
    let mut cases = Vec::new();
    for &later in TIMESTAMPS {
        for &earlier in TIMESTAMPS {
            let difference = PgInterval::timestamp_difference(
                parse_timestamp(later).unwrap_or_else(|| panic!("{later:?}")),
                parse_timestamp(earlier).unwrap_or_else(|| panic!("{earlier:?}")),
            );
            cases.push((
                format!(
                    "SELECT ({}::timestamp - {}::timestamp)::text",
                    quoted(later),
                    quoted(earlier)
                ),
                worded(difference),
            ));
        }
    }
    compare(&mut conn, cases, as_printed);
}

#[test]
fn pg18_shifts_every_time() {
    let mut conn = connect();
    let mut cases = Vec::new();
    for &at in TIMES {
        let time = parse_time(at).unwrap_or_else(|| panic!("{at:?}"));
        for &span in OPERANDS {
            for (operator, result) in [
                ("+", interval(span).add_to_time(time)),
                ("-", interval(span).subtract_from_time(time)),
            ] {
                cases.push((
                    format!(
                        "SELECT ({}::time {operator} {}::interval)::text",
                        quoted(at),
                        quoted(span)
                    ),
                    worded(result),
                ));
            }
        }
    }
    compare(&mut conn, cases, time_answer);
}

/// `sum` and `avg` of every group, folded in and also folded with an infinity and a total near a
/// field's minimum that then leave again.
#[test]
fn pg18_sums_and_averages_every_group() {
    let mut conn = connect();
    let mut cases = Vec::new();
    for &group in GROUPS {
        let array = format!(
            "ARRAY[{}]::interval[]",
            group
                .iter()
                .map(|text| quoted(text))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let mut total = IntervalSum::default();
        for &text in group {
            total.merge(IntervalSum::of(interval(text), 1));
        }
        let mut churned = total;
        for weight in [1, -1] {
            churned.merge(IntervalSum::of(PgInterval::NEG_INFINITY, weight));
            churned.merge(IntervalSum::of(
                interval("-178956970 years -8 mons"),
                weight,
            ));
        }
        for (aggregate, got) in [
            ("sum", total.sum()),
            ("avg", total.avg()),
            ("sum", churned.sum()),
            ("avg", churned.avg()),
        ] {
            cases.push((
                format!("SELECT {aggregate}(x)::text FROM unnest({array}) AS x"),
                got.map(|value| value.map(|value| value.to_string()))
                    .map_err(|error| error.to_string()),
            ));
        }
    }
    compare(&mut conn, cases, as_printed);
}
