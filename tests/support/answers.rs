//! PostgreSQL's answers to many one-value queries in one round trip, compared with the crate's.
//!
//! Include in a test binary next to `containers`, `exit_slot` and `postgres`:
//!
//! ```rust,ignore
//! #[path = "support/answers.rs"]
//! mod answers;
//! ```

use std::fmt::Display;

use diesel::{
    PgConnection, RunQueryDsl,
    dsl::sql,
    sql_types::{Array, Text},
};

use crate::postgres;

/// What a query of one text value answers, its value, SQL `NULL`, or the message of the error it
/// raises.
#[derive(Debug, PartialEq, Eq)]
pub enum Answer {
    Value(String),
    Null,
    Error(String),
}

impl Answer {
    /// The crate's answer in PostgreSQL's words, its value printed or its error's message.
    pub fn of<T: Display, E: Display>(result: Result<T, E>) -> Self {
        match result {
            Ok(value) => Self::Value(value.to_string()),
            Err(error) => Self::Error(error.to_string()),
        }
    }
}

/// A connection to the container in `slot` whose `pg_temp.answer` runs one query and answers its
/// text, or `ERROR: ` and the message it raises.
pub fn connect(slot: &'static postgres::Slot, tag: &str) -> PgConnection {
    let mut conn = postgres::connect(slot, tag);
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
                Answer::Value(value.to_owned())
            } else if let Some(message) = text.strip_prefix("ERROR: ") {
                Answer::Error(message.to_owned())
            } else {
                Answer::Null
            }
        })
        .collect()
}

/// Every query against the crate's answer for it, listing the disagreements.
pub fn compare(conn: &mut PgConnection, cases: Vec<(String, Answer)>) {
    let (queries, got): (Vec<String>, Vec<Answer>) = cases.into_iter().unzip();
    let expected = answers(conn, &queries);
    let mismatches: Vec<String> = queries
        .iter()
        .zip(got)
        .zip(expected)
        .filter(|((_, got), expected)| got != expected)
        .map(|((query, got), expected)| format!("{query}: crate {got:?}, PostgreSQL {expected:?}"))
        .collect();
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

/// `text` quoted as an SQL string literal.
pub fn quoted(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}
