use diesel::prelude::*;
use diesel::sql_types::Text;
use sql_values::{
    PgDate, PgTime, PgTimestamp, parse_bool, parse_date, parse_f64, parse_i64, parse_time,
    parse_timestamp,
};

diesel::table! {
    scalars (id) {
        id -> Integer,
        txt -> Text,
    }
}

use scalars::dsl as s;

#[derive(Insertable)]
#[diesel(table_name = scalars)]
struct Row {
    id: i32,
    txt: String,
}

fn mem_conn() -> SqliteConnection {
    let mut conn = SqliteConnection::establish(":memory:").expect("open sqlite memory db");
    // DDL: Diesel migration DSL does not cover CREATE TABLE for ad-hoc schemas.
    diesel::sql_query("CREATE TABLE scalars (id INTEGER PRIMARY KEY, txt TEXT NOT NULL)")
        .execute(&mut conn)
        .expect("create scalars table");
    conn
}

fn sqlite_text(conn: &mut SqliteConnection, expr: &str) -> String {
    diesel::select(diesel::dsl::sql::<Text>(expr))
        .get_result(conn)
        .unwrap_or_else(|e| panic!("sqlite_text({expr}): {e}"))
}

fn read_txt(conn: &mut SqliteConnection, id: i32) -> String {
    s::scalars
        .select(s::txt)
        .filter(s::id.eq(id))
        .first(conn)
        .unwrap_or_else(|e| panic!("read id={id}: {e}"))
}

#[test]
fn sqlite_timestamp_iso_text() {
    let mut conn = mem_conn();

    let rows: &[(i32, &str, PgTimestamp)] = &[
        (
            1,
            "2024-01-15 10:30:45",
            PgTimestamp::new(
                PgDate::from_ymd(2024, 1, 15).unwrap(),
                PgTime::from_hms_micro(10, 30, 45, 0).unwrap(),
            )
            .unwrap(),
        ),
        (
            2,
            "2024-01-15 10:30:45.123456",
            PgTimestamp::new(
                PgDate::from_ymd(2024, 1, 15).unwrap(),
                PgTime::from_hms_micro(10, 30, 45, 123_456).unwrap(),
            )
            .unwrap(),
        ),
        (
            3,
            "2024-02-29 23:59:59",
            PgTimestamp::new(
                PgDate::from_ymd(2024, 2, 29).unwrap(),
                PgTime::from_hms_micro(23, 59, 59, 0).unwrap(),
            )
            .unwrap(),
        ),
    ];

    for (id, txt, expected) in rows {
        diesel::insert_into(s::scalars)
            .values(Row {
                id: *id,
                txt: txt.to_string(),
            })
            .execute(&mut conn)
            .expect("insert row");

        let read = read_txt(&mut conn, *id);
        let parsed = parse_timestamp(&read).expect("parse timestamp");
        assert_eq!(parsed, *expected, "id={id} text={txt}");
    }
}

#[test]
fn sqlite_date_iso_text() {
    let mut conn = mem_conn();

    let rows: &[(i32, &str, PgDate)] = &[
        (1, "2024-01-15", PgDate::from_ymd(2024, 1, 15).unwrap()),
        (2, "2024-02-29", PgDate::from_ymd(2024, 2, 29).unwrap()),
        (3, "2000-01-01", PgDate::from_ymd(2000, 1, 1).unwrap()),
    ];

    for (id, txt, expected) in rows {
        diesel::insert_into(s::scalars)
            .values(Row {
                id: *id,
                txt: txt.to_string(),
            })
            .execute(&mut conn)
            .expect("insert row");

        let read = read_txt(&mut conn, *id);
        let parsed = parse_date(&read).expect("parse date");
        assert_eq!(parsed, *expected, "id={id} text={txt}");
    }
}

#[test]
fn sqlite_year_zero_and_before() {
    let mut conn = mem_conn();

    let text = sqlite_text(&mut conn, "date('0000-02-29')");
    assert_eq!(parse_date(&text), PgDate::from_ymd(0, 2, 29), "{text:?}");

    let text = sqlite_text(&mut conn, "datetime('0000-01-01 12:00:00')");
    let expected = PgTimestamp::new(
        PgDate::from_ymd(0, 1, 1).unwrap(),
        PgTime::from_hms_micro(12, 0, 0, 0).unwrap(),
    );
    assert_eq!(parse_timestamp(&text), expected, "{text:?}");

    // PostgreSQL and MySQL both reject a signed year on input.
    let text = sqlite_text(&mut conn, "date('0000-01-01', '-1 day')");
    assert_eq!(text, "-0001-12-31");
    assert_eq!(parse_date(&text), None);
}

#[test]
fn sqlite_time_iso_text() {
    let mut conn = mem_conn();

    let rows: &[(i32, &str, PgTime)] = &[
        (
            1,
            "10:30:45",
            PgTime::from_hms_micro(10, 30, 45, 0).unwrap(),
        ),
        (
            2,
            "10:30:45.123456",
            PgTime::from_hms_micro(10, 30, 45, 123_456).unwrap(),
        ),
        (
            3,
            "23:59:59",
            PgTime::from_hms_micro(23, 59, 59, 0).unwrap(),
        ),
    ];

    for (id, txt, expected) in rows {
        diesel::insert_into(s::scalars)
            .values(Row {
                id: *id,
                txt: txt.to_string(),
            })
            .execute(&mut conn)
            .expect("insert row");

        let read = read_txt(&mut conn, *id);
        let parsed = parse_time(&read).expect("parse time");
        assert_eq!(parsed, *expected, "id={id} text={txt}");
    }
}

#[test]
fn sqlite_bool_convention() {
    let mut conn = mem_conn();

    let rows: &[(i32, &str, bool)] = &[(1, "1", true), (2, "0", false)];

    for (id, txt, expected) in rows {
        diesel::insert_into(s::scalars)
            .values(Row {
                id: *id,
                txt: txt.to_string(),
            })
            .execute(&mut conn)
            .expect("insert row");

        let read = read_txt(&mut conn, *id);
        let parsed = parse_bool(&read).expect("parse bool");
        assert_eq!(parsed, *expected, "id={id} text={txt}");
    }
}

#[test]
fn sqlite_integer_and_float_text() {
    let mut conn = mem_conn();

    let i64_cases: &[(&str, i64)] = &[
        ("CAST(0 AS TEXT)", 0),
        ("CAST(42 AS TEXT)", 42),
        ("CAST(-42 AS TEXT)", -42),
        ("CAST(9223372036854775807 AS TEXT)", i64::MAX),
        ("CAST(-9223372036854775808 AS TEXT)", i64::MIN),
    ];
    for &(expr, expected) in i64_cases {
        let text = sqlite_text(&mut conn, expr);
        let got = parse_i64(&text).unwrap_or_else(|| panic!("parse_i64: {text:?} (expr={expr})"));
        assert_eq!(got, expected, "expr={expr} text={text:?}");
    }

    let f64_cases: &[(&str, f64)] = &[
        ("CAST(1.5 AS TEXT)", 1.5),
        ("CAST(-2.5 AS TEXT)", -2.5),
        ("CAST(1e20 AS TEXT)", 1e20_f64),
    ];
    for &(expr, expected) in f64_cases {
        let text = sqlite_text(&mut conn, expr);
        let got = parse_f64(&text).unwrap_or_else(|| panic!("parse_f64: {text:?} (expr={expr})"));
        assert_eq!(got, expected, "expr={expr} text={text:?}");
    }
}
