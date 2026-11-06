#[path = "support/containers.rs"]
mod containers;
#[path = "support/exit_slot.rs"]
mod exit_slot;
#[path = "support/fractional_matrix.rs"]
mod fractional_matrix;
#[path = "support/postgres.rs"]
mod postgres;

use diesel::{dsl::sql, prelude::*, result::Error as DieselError, sql_types::Text};
use sql_values::{
    DateTimeError, InputContext, PgDate, PgTime, PgTimeTz, PgTimeZone, PgTimestamp, PgTimestampTz,
    TimeZoneAbbreviations, parse_bool, parse_date, parse_f64, parse_i64, parse_pg_bytea_hex,
    parse_time, parse_timestamp, parse_timestamp_tz,
};

/// One container per test binary, removed when the binary exits; a plain
/// `static` would keep it running forever, see `exit_slot`.
static PG: postgres::Slot = exit_slot::ExitSlot::new();

fn pg_conn() -> PgConnection {
    postgres::connect(&PG, "16.15-alpine")
}

static PG18: postgres::Slot = exit_slot::ExitSlot::new();

/// A PostgreSQL 18.6 session under `TimeZone` `UTC`.
fn pg18_conn() -> PgConnection {
    let mut conn = postgres::connect(&PG18, "18.6");
    set_timezone(&mut conn, "UTC");
    conn
}

/// What `expression` prints through its type's output function on PostgreSQL 18.6, or the
/// message it raises without the quoted input PostgreSQL appends.
fn pg18_answer(conn: &mut PgConnection, expression: &str) -> Result<String, String> {
    // Raw SQL with the literal inline, so that it reaches the type's input function.
    match diesel::select(sql::<Text>(&format!("format('%s', {expression})"))).get_result(conn) {
        Ok(text) => Ok(text),
        Err(DieselError::DatabaseError(_, info)) => Err(info
            .message()
            .split_once(": \"")
            .map_or(info.message(), |(stem, _)| stem)
            .to_owned()),
        Err(other) => panic!("{expression}: {other}"),
    }
}

/// `text` quoted as an SQL string literal.
fn quoted(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// The crate's answer in PostgreSQL's words, its value printed or its error's message.
fn worded<T: std::fmt::Display, E: std::fmt::Display>(
    result: Result<T, E>,
) -> Result<String, String> {
    result
        .map(|value| value.to_string())
        .map_err(|error| error.to_string())
}

/// Every expression against the crate's answer for it, listing the disagreements.
fn assert_answers(conn: &mut PgConnection, cases: Vec<(String, Result<String, String>)>) {
    let mismatches: Vec<String> = cases
        .into_iter()
        .filter_map(|(expression, got)| {
            let expected = pg18_answer(conn, &expression);
            (got != expected)
                .then(|| format!("{expression}: crate {got:?}, PostgreSQL {expected:?}"))
        })
        .collect();
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

/// Time literals at the clock's bounds, rounding, offsets and refusals.
const TIMES: &[&str] = &[
    "00:00",
    "12:00",
    "12:34:56",
    "12:34:56.789",
    " 12:34:56.789012 ",
    "12:00:00.1234565",
    "12:00:00.1234575",
    "12:00:00.0000005",
    "12:00:00.0000015",
    "23:59:59.9999995",
    "23:59:59.999999",
    "24:00",
    "24:00:00",
    "23:59:60",
    "24:00:00.000001",
    "23:59:60.000001",
    "24:00:01",
    "25:00:00",
    "12:60:00",
    "12:00:61",
    "25:00+16",
    "12:61+16",
    "allballs",
    "ALLBALLS",
    "12:00:00+05",
    "12:00:00-05:30",
    "12:00:00+05:30:15",
    "12:00:00-15:59:59",
    "12:00:00+16",
    "12:00:00+0530",
    "12:00:00+123",
    "12:00:00+0560",
    "12:00:00 +05",
    "12:00Z",
    "12:00 z",
    "24:00:00+00",
    "12:00 CEST",
    "12:00:00EST",
    "12:00 z",
    "12:00 zulu",
    "12:00 MSK",
    "12:00 factory",
];

#[test]
fn pg18_reads_every_time_literal() {
    let mut conn = pg18_conn();
    let mut cases = Vec::new();
    for &text in TIMES {
        cases.push((
            format!("{}::time", quoted(text)),
            worded(PgTime::parse(text, InputContext::new())),
        ));
        let timetz = PgTimeTz::parse(text, InputContext::new());
        if timetz != Err(DateTimeError::Unsupported) {
            cases.push((format!("{}::timetz", quoted(text)), worded(timetz)));
        }
    }
    assert_answers(&mut conn, cases);
}

/// `timetz_cmp` on every pair of the time literals that read as `timetz`.
#[test]
fn pg18_orders_every_timetz_pair() {
    let mut conn = pg18_conn();
    let values: Vec<(&str, PgTimeTz)> = TIMES
        .iter()
        .filter_map(|&text| Some((text, PgTimeTz::parse(text, InputContext::new()).ok()?)))
        .collect();
    let mut cases = Vec::new();
    for &(left, left_value) in &values {
        for &(right, right_value) in &values {
            // `Ordering`'s discriminants are -1, 0 and 1, as `timetz_cmp` answers.
            let order = left_value.cmp(&right_value) as i8;
            cases.push((
                format!(
                    "timetz_cmp({}::timetz, {}::timetz)",
                    quoted(left),
                    quoted(right)
                ),
                Ok(order.to_string()),
            ));
        }
    }
    assert_answers(&mut conn, cases);
}

/// Zones PostgreSQL's `TimeZone` names: daylight saving in the north and in the south, a zone
/// that dropped it, fixed offsets in both sign conventions, and POSIX rules with and without a
/// rule given.
const ZONES: &[&str] = &[
    "UTC",
    "Europe/Rome",
    "Australia/Sydney",
    "America/Sao_Paulo",
    "America/New_York",
    "Asia/Kolkata",
    "+02",
    "-03:30",
    "EST5EDT,M3.2.0,M11.1.0",
    "XXX3YYY",
    "<-03>3<-02>,M10.1.0/0,M2.3.0/0",
    "ABC3DEF,J60/2,J60/3",
];

/// The zones of [`ZONES`] that need no time zone database, and so hold over PostgreSQL's whole
/// range.
const RULE_ZONES: &[&str] = &[
    "UTC",
    "+02",
    "-03:30",
    "EST5EDT,M3.2.0,M11.1.0",
    "XXX3YYY",
    "<-03>3<-02>,M10.1.0/0,M2.3.0/0",
    "ABC3DEF,J60/2,J60/3",
];

/// Local clocks in each zone's 2026 gaps and overlaps, on either side of them, at local mean
/// time, and at PostgreSQL's first instant.
const LOCAL_CLOCKS: &[&str] = &[
    "2026-03-29 01:59:59",
    "2026-03-29 02:00:00",
    "2026-03-29 02:30:00",
    "2026-03-29 03:00:00",
    "2026-10-25 01:59:59.999999",
    "2026-10-25 02:00:00",
    "2026-10-25 02:30:00",
    "2026-10-25 03:00:00",
    "2026-04-05 02:30:00",
    "2026-04-05 03:00:00",
    "2026-10-04 02:30:00",
    "2026-03-08 02:30:00",
    "2026-11-01 01:30:00",
    "1850-01-01 00:00:00",
    "4714-11-24 00:00:00 BC",
    "4714-11-24 23:59:59 BC",
    "4714-10-31 23:00:00 BC",
    "2026-07-01 12:00:00",
    "2026-10-04 00:30:00",
    "2026-02-15 00:30:00",
];

/// Instants around each zone's 2026 changes, at local mean time, and at both ends of
/// PostgreSQL's range.
const INSTANTS: &[&str] = &[
    "2026-03-29 00:59:59+00",
    "2026-03-29 01:00:00+00",
    "2026-10-25 00:30:00+00",
    "2026-10-25 01:30:00+00",
    "2026-04-04 15:30:00+00",
    "2026-10-03 16:00:00+00",
    "2026-03-08 06:59:59+00",
    "2026-03-08 07:00:00+00",
    "1850-01-01 00:00:00+00",
    "4714-11-24 00:00:00+00 BC",
    "4714-11-24 03:00:00+00 BC",
    "infinity",
    "-infinity",
];

/// Instants and clocks past jiff's years, which only the zones that need no database read.
const FAR: &[&str] = &[
    "294276-12-31 23:59:59+00",
    "294276-12-31 22:00:00+00",
    "200000-06-15 12:00:00+00",
];

/// Each zone with the crate's value for it.
fn zones() -> Vec<(&'static str, PgTimeZone)> {
    ZONES
        .iter()
        .map(|&name| {
            let zone = PgTimeZone::parse(name).unwrap_or_else(|error| panic!("{name:?}: {error}"));
            (name, zone)
        })
        .collect()
}

#[test]
fn pg18_reads_local_timestamptz_in_each_zone() {
    let mut conn = pg18_conn();
    let utc = PgTimeZone::parse("UTC").expect("UTC");
    for (name, zone) in zones() {
        set_timezone(&mut conn, name);
        let far: Vec<String> = if RULE_ZONES.contains(&name) {
            FAR.iter()
                .map(|at| at.trim_end_matches("+00").to_owned())
                .collect()
        } else {
            Vec::new()
        };
        let mut cases = Vec::new();
        for text in LOCAL_CLOCKS.iter().map(|&at| at.to_owned()).chain(far) {
            let context = InputContext::new().with_time_zone(&zone);
            let got = PgTimestampTz::parse(&text, context).and_then(|at| at.to_timestamp(&utc));
            cases.push((
                format!("({}::timestamptz AT TIME ZONE 'UTC')", quoted(&text)),
                worded(got),
            ));
        }
        assert_answers(&mut conn, cases);
    }
}

#[test]
fn pg18_converts_between_timestamp_timestamptz_and_date_in_each_zone() {
    let mut conn = pg18_conn();
    let utc = PgTimeZone::parse("UTC").expect("UTC");
    for (name, zone) in zones() {
        set_timezone(&mut conn, name);
        let far: &[&str] = if RULE_ZONES.contains(&name) { FAR } else { &[] };
        let mut cases = Vec::new();
        for &text in INSTANTS.iter().chain(far) {
            let at = PgTimestampTz::parse(text, InputContext::new())
                .unwrap_or_else(|error| panic!("{text:?}: {error}"));
            cases.push((
                format!("{}::timestamptz::timestamp", quoted(text)),
                worded(at.to_timestamp(&zone)),
            ));
            cases.push((
                format!("{}::timestamptz::date", quoted(text)),
                worded(at.to_date(&zone)),
            ));
            let local = text.trim_end_matches("+00").replace("+00 BC", " BC");
            let clock = PgTimestamp::parse(&local, InputContext::new())
                .unwrap_or_else(|error| panic!("{local:?}: {error}"));
            cases.push((
                format!(
                    "({}::timestamp::timestamptz AT TIME ZONE 'UTC')",
                    quoted(&local)
                ),
                worded(
                    clock
                        .to_timestamptz(&zone)
                        .and_then(|at| at.to_timestamp(&utc)),
                ),
            ));
        }
        assert_answers(&mut conn, cases);
    }
}

/// `TimeZone` settings at each branch of `check_timezone`, with the Debian image's legacy links
/// left out, since its tzdata omits them.
const SETTINGS: &[&str] = &[
    "UTC",
    "utc",
    "Etc/UTC",
    "GMT",
    "Europe/Rome",
    "europe/rome",
    "EUROPE/ROME",
    "Asia/Kolkata",
    "PST8PDT",
    "EST5EDT",
    "+02",
    "-02",
    "+2",
    "02",
    "2.5",
    "1e1",
    "0x10",
    " +02",
    "+15",
    "+167",
    "+168",
    "200",
    "-0",
    "inf",
    "nan",
    "+02:00",
    "+14:30",
    "UTC+2",
    "UTC-2",
    "UTC0",
    "JST-9",
    "GMT-14",
    "Etc/GMT-14",
    "<+02>-2",
    "<+02>-2<+03>",
    "<+02>",
    "A",
    "AB3",
    "ABC3",
    "abc3",
    "ABC-30",
    "ABC25",
    "ABC167",
    "ABC168",
    "ABC3:00:01",
    "ABC+24:59:59",
    "XXX3YYY",
    "XXX3",
    "ABC3DEF,J60,J300",
    "ABC3DEF,60,300",
    "ABC3DEF4,M3.5.0/26,M10.5.0",
    "ABC3DEF4,M3.5.0/-170,M10.5.0",
    "ABC3DEF4,M3.5.0/168,M10.5.0",
    "EST5EDT,M3.2.0",
    "<-03>3<-02>,M3.5.0/-2,M10.5.0/-1",
    "interval '+02:00'",
    "interval '-01:30:15'",
    "interval '1 day'",
    "interval '1 mon'",
    "interval '167:59:59'",
    "interval '168:00:00'",
    "interval 'abc'",
    "interval '+02:00",
    "+-5",
    "ABC+",
    "ABC3DEF,Mx",
    "ABC3DEF,M3,M10.5.0",
    "ABC3DEF,M3.2,M11.1.0",
    "ABC3DEF,M3.2.0",
    "ABC3DEF,M3.2.0,M11.1.0x",
    "ABC3DEF4;M3.2.0,M11.1.0",
    "ABC3DEF,0/0,365/26",
    "ABC3DEF,J60/2,J60/3",
    "ABC3+4",
    "ABC3<>",
    "ABC3DEF4X",
    ":UTC",
    ":Europe/Rome",
    ":foo",
    ":ABC3",
    "::UTC",
    "foo",
    "foo bar",
    "",
];

#[test]
fn pg18_reads_every_time_zone_setting() {
    let mut conn = pg18_conn();
    let too_long = "A".repeat(256) + "3";
    let mismatches: Vec<String> = SETTINGS
        .iter()
        .copied()
        .chain([too_long.as_str()])
        .filter_map(|setting| {
            // Raw SQL: `set_config` reports the GUC's refusal, and `SHOW` its canonical name.
            let expected = diesel::select(sql::<Text>(&format!(
                "set_config('TimeZone', {}, true) || '|' || current_setting('TimeZone')",
                quoted(setting)
            )))
            .get_result::<String>(&mut conn)
            .ok()
            .and_then(|text| text.split_once('|').map(|(_, name)| name.to_owned()));
            let got = PgTimeZone::parse(setting)
                .ok()
                .map(|zone| zone.name().to_owned());
            (got != expected)
                .then(|| format!("{setting:?}: crate {got:?}, PostgreSQL {expected:?}"))
        })
        .collect();
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

/// Clocks at which the sets' abbreviations are read, on either side of Moscow's changes of 2011
/// and 2014.
const ABBREVIATION_CLOCKS: &[&str] = &["2010-07-15 12:00", "2012-01-15 12:00", "2026-01-15 12:00"];

/// Every abbreviation of each bundled set, as PostgreSQL lists it, read at each clock.
#[test]
fn pg18_reads_every_abbreviation_of_each_set() {
    let mut conn = pg18_conn();
    let utc = PgTimeZone::parse("UTC").expect("UTC");
    for name in ["Default", "Australia", "India"] {
        let set = TimeZoneAbbreviations::bundled(name).expect("a bundled set");
        // Raw SQL: `SET` and `pg_timezone_abbrevs_abbrevs` have no DSL form.
        diesel::sql_query(format!("SET timezone_abbreviations = {}", quoted(name)))
            .execute(&mut conn)
            .expect("set the abbreviations");
        let abbreviations: Vec<String> = diesel::select(sql::<diesel::sql_types::Array<Text>>(
            "ARRAY(SELECT abbrev FROM pg_timezone_abbrevs_abbrevs())",
        ))
        .get_result(&mut conn)
        .expect("list the abbreviations");
        assert!(abbreviations.len() > 190, "{name}: {}", abbreviations.len());
        let context = InputContext::new().with_time_zone_abbreviations(set);
        let mut cases = Vec::new();
        for abbreviation in &abbreviations {
            for clock in ABBREVIATION_CLOCKS {
                let literal = format!("{clock} {abbreviation}");
                let got =
                    PgTimestampTz::parse(&literal, context).and_then(|at| at.to_timestamp(&utc));
                cases.push((
                    format!("({}::timestamptz AT TIME ZONE 'UTC')", quoted(&literal)),
                    worded(got),
                ));
            }
        }
        assert_answers(&mut conn, cases);
    }
}

/// Abbreviations the session's zone uses, which PostgreSQL reads before the set's, and one
/// neither knows.
#[test]
fn pg18_reads_abbreviations_of_the_session_zone() {
    let mut conn = pg18_conn();
    let utc = PgTimeZone::parse("UTC").expect("UTC");
    let cases = [
        ("Asia/Kolkata", "2026-01-15 12:00 IST"),
        ("UTC", "2026-01-15 12:00 IST"),
        ("UTC", "2026-01-15 12:00 UTC"),
        ("GMT", "2026-01-15 12:00 GMT"),
        ("EST5EDT,M3.2.0,M11.1.0", "2026-07-15 12:00 EST"),
        ("Europe/Rome", "1880-01-01 12:00 RMT"),
        ("Europe/Rome", "1850-01-01 12:00 LMT"),
        ("Europe/Rome", "2026-01-15 12:00 CEST"),
        ("Europe/Moscow", "2010-07-15 12:00 MSK"),
        ("Europe/Moscow", "2012-01-15 12:00 MSK"),
        ("Europe/Moscow", "2012-01-15 12:00 MSD"),
        ("America/New_York", "2026-07-15 12:00 EST"),
        ("America/New_York", "1880-01-01 12:00 LMT"),
        ("EST5EDT,M3.2.0,M11.1.0", "2026-07-15 12:00 EDT"),
        ("+02", "2026-07-15 12:00 CEST"),
    ];
    let mut checked = Vec::new();
    for (name, literal) in cases {
        let zone = PgTimeZone::parse(name).unwrap_or_else(|error| panic!("{name}: {error}"));
        set_timezone(&mut conn, name);
        let context = InputContext::new().with_time_zone(&zone);
        let got = PgTimestampTz::parse(literal, context).and_then(|at| at.to_timestamp(&utc));
        let expression = format!("({}::timestamptz AT TIME ZONE 'UTC')", quoted(literal));
        checked.push((name, expression.clone(), worded(got)));
    }
    let mismatches: Vec<String> = checked
        .into_iter()
        .filter_map(|(name, expression, got)| {
            set_timezone(&mut conn, name);
            let expected = pg18_answer(&mut conn, &expression);
            (got != expected)
                .then(|| format!("{name} {expression}: crate {got:?}, PostgreSQL {expected:?}"))
        })
        .collect();
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
    // An abbreviation nobody knows is refused, and the crate declines rather than guesses.
    let context = InputContext::new().with_time_zone(&utc);
    assert_eq!(
        PgTimestampTz::parse("1880-01-01 12:00 RMT", context),
        Err(DateTimeError::Unsupported)
    );
    set_timezone(&mut conn, "UTC");
    assert!(pg18_answer(&mut conn, "'1880-01-01 12:00 RMT'::timestamptz").is_err());
}

// Raw SQL: vendor cast syntax cannot be expressed in the Diesel DSL.
fn pg_text(conn: &mut PgConnection, expr: &str) -> String {
    diesel::select(diesel::dsl::sql::<Text>(expr))
        .get_result(conn)
        .unwrap_or_else(|e| panic!("pg_text({expr}): {e}"))
}

fn set_timezone(conn: &mut PgConnection, zone: &str) {
    // Raw SQL: SET timezone is a session command with no DSL equivalent.
    diesel::sql_query(format!("SET timezone = '{zone}'"))
        .execute(conn)
        .unwrap_or_else(|e| panic!("SET timezone={zone}: {e}"));
}

#[test]
fn pg_timestamp_fractional_precision_matrix() {
    let mut conn = pg_conn();

    let cases: &[(&str, u32)] = &[
        ("'2024-01-15 10:30:45'::timestamp(0)::text", 0),
        ("'2024-01-15 10:30:45.1'::timestamp(1)::text", 100_000_000),
        ("'2024-01-15 10:30:45.12'::timestamp(2)::text", 120_000_000),
        ("'2024-01-15 10:30:45.123'::timestamp(3)::text", 123_000_000),
        (
            "'2024-01-15 10:30:45.1234'::timestamp(4)::text",
            123_400_000,
        ),
        (
            "'2024-01-15 10:30:45.12345'::timestamp(5)::text",
            123_450_000,
        ),
        (
            "'2024-01-15 10:30:45.123456'::timestamp(6)::text",
            123_456_000,
        ),
    ];

    fractional_matrix::assert_fractional_seconds(cases, |expr| pg_text(&mut conn, expr));
}

#[test]
fn pg_timestamptz_offset_forms() {
    let mut conn = pg_conn();

    let utc_instant = "2024-06-15 12:00:00+00";
    let pg_literal = "'2024-06-15 12:00:00+00'::timestamptz::text";

    // DST-free zones covering +00, +05:00, +05:30, and -08:00 offset forms.
    let zones: &[&str] = &["UTC", "Etc/GMT-5", "Asia/Kolkata", "Etc/GMT+8"];

    for &zone in zones {
        set_timezone(&mut conn, zone);
        let text = pg_text(&mut conn, pg_literal);
        let got = parse_timestamp_tz(&text)
            .unwrap_or_else(|| panic!("parse_timestamp_tz: {text:?} (zone={zone})"));
        assert_eq!(got.to_string(), utc_instant, "zone={zone} text={text:?}");
    }
}

#[test]
fn pg_timestamptz_local_mean_time_offsets() {
    let mut conn = pg_conn();

    // Before a zone adopted standard time PostgreSQL prints its local mean
    // time, whose offset carries seconds.
    let cases: &[(&str, &str, &str)] = &[
        ("Europe/Brussels", "1850-01-01 00:00:00+00", "+00:17:30"),
        (
            "Europe/Dublin",
            "1870-01-01 00:00:00.544453+00",
            "-00:25:21",
        ),
        ("America/New_York", "1850-01-01 00:00:00+00", "-04:56:02"),
    ];

    for &(zone, instant, offset) in cases {
        set_timezone(&mut conn, zone);
        let text = pg_text(&mut conn, &format!("'{instant}'::timestamptz::text"));
        assert!(text.ends_with(offset), "zone={zone} text={text:?}");
        let got = parse_timestamp_tz(&text)
            .unwrap_or_else(|| panic!("parse_timestamp_tz: {text:?} (zone={zone})"));
        assert_eq!(got.to_string(), instant, "zone={zone} text={text:?}");
    }
}

#[test]
fn pg_years_past_9999_and_before_1_ad() {
    let mut conn = pg_conn();
    set_timezone(&mut conn, "UTC");

    for (expr, expected) in [
        (
            "'9999-12-31 23:00:00-01'::timestamptz",
            "10000-01-01 00:00:00+00",
        ),
        (
            "'0001-01-01 00:30:00+01'::timestamptz",
            "0001-12-31 23:30:00+00 BC",
        ),
    ] {
        let text = pg_text(&mut conn, &format!("{expr}::text"));
        let got = parse_timestamp_tz(&text).map(|at| at.to_string());
        assert_eq!(got.as_deref(), Some(expected), "{text:?}");
    }

    let text = pg_text(&mut conn, "'0044-03-15 12:00:00 BC'::timestamp::text");
    let got = parse_timestamp(&text).map(|at| at.to_string());
    assert_eq!(got.as_deref(), Some("0044-03-15 12:00:00 BC"), "{text:?}");

    for (expr, expected) in [
        ("'123456-01-01'::date", "123456-01-01"),
        ("'0001-01-01 BC'::date", "0001-01-01 BC"),
    ] {
        let text = pg_text(&mut conn, &format!("{expr}::text"));
        let got = parse_date(&text).map(|date| date.to_string());
        assert_eq!(got.as_deref(), Some(expected), "{text:?}");
    }
}

#[test]
fn pg_scalar_types() {
    let mut conn = pg_conn();

    let date_text = pg_text(&mut conn, "'2024-03-01'::date::text");
    let got_date = parse_date(&date_text).unwrap_or_else(|| panic!("parse_date: {date_text:?}"));
    assert_eq!(got_date, PgDate::from_ymd(2024, 3, 1).unwrap());

    let time_text = pg_text(&mut conn, "'14:05:06.789012'::time::text");
    let got_time = parse_time(&time_text).unwrap_or_else(|| panic!("parse_time: {time_text:?}"));
    assert_eq!(
        got_time,
        PgTime::from_hms_micro(14, 5, 6, 789_012).unwrap(),
        "time text={time_text:?}"
    );

    // PostgreSQL's Boolean text cast differs from its wire spelling.
    let t_text = pg_text(&mut conn, "CASE WHEN true THEN 't' ELSE 'f' END");
    let f_text = pg_text(&mut conn, "CASE WHEN false THEN 't' ELSE 'f' END");
    assert_eq!(parse_bool(&t_text), Some(true), "bool true: {t_text:?}");
    assert_eq!(parse_bool(&f_text), Some(false), "bool false: {f_text:?}");

    let bytea_text = pg_text(&mut conn, r"'\xDEADBEEF'::bytea::text");
    let got_bytes = parse_pg_bytea_hex(&bytea_text)
        .unwrap_or_else(|| panic!("parse_pg_bytea_hex: {bytea_text:?}"));
    assert_eq!(got_bytes, vec![0xde, 0xad, 0xbe, 0xef]);

    let empty_text = pg_text(&mut conn, r"'\x'::bytea::text");
    assert_eq!(parse_pg_bytea_hex(&empty_text), Some(vec![]));
}

#[cfg(feature = "decimal")]
#[test]
fn pg_decimal_beyond_f64_precision() {
    use bigdecimal::BigDecimal;
    use core::str::FromStr;
    use sql_values::parse_decimal;

    let mut conn = pg_conn();
    let expr = "'12345678901234567890.123456789'::numeric::text";
    let text = pg_text(&mut conn, expr);
    let got = parse_decimal(&text).unwrap_or_else(|| panic!("parse_decimal: {text:?}"));
    let expected = BigDecimal::from_str("12345678901234567890.123456789").unwrap();
    assert_eq!(got, expected, "decimal text={text:?}");
}

#[test]
fn pg_integer_and_float_text() {
    let mut conn = pg_conn();

    let i64_cases: &[(&str, i64)] = &[
        ("'0'::bigint::text", 0),
        ("'42'::bigint::text", 42),
        ("'-42'::bigint::text", -42),
        ("'9223372036854775807'::bigint::text", i64::MAX),
        ("'-9223372036854775808'::bigint::text", i64::MIN),
    ];
    for &(expr, expected) in i64_cases {
        let text = pg_text(&mut conn, expr);
        let got = parse_i64(&text).unwrap_or_else(|| panic!("parse_i64: {text:?} (expr={expr})"));
        assert_eq!(got, expected, "expr={expr} text={text:?}");
    }

    let f64_finite_cases: &[(&str, f64)] = &[
        ("'0'::float8::text", 0.0),
        ("'2.5'::float8::text", 2.5_f64),
        ("'-1.5'::float8::text", -1.5_f64),
        ("'1e20'::float8::text", 1e20_f64),
    ];
    for &(expr, expected) in f64_finite_cases {
        let text = pg_text(&mut conn, expr);
        let got = parse_f64(&text).unwrap_or_else(|| panic!("parse_f64: {text:?} (expr={expr})"));
        assert_eq!(got, expected, "expr={expr} text={text:?}");
    }

    let inf_text = pg_text(&mut conn, "'Infinity'::float8::text");
    let got_inf =
        parse_f64(&inf_text).unwrap_or_else(|| panic!("parse_f64 Infinity: {inf_text:?}"));
    assert!(
        got_inf.is_infinite() && got_inf.is_sign_positive(),
        "text={inf_text:?}"
    );

    let neg_inf_text = pg_text(&mut conn, "'-Infinity'::float8::text");
    let got_neg =
        parse_f64(&neg_inf_text).unwrap_or_else(|| panic!("parse_f64 -Infinity: {neg_inf_text:?}"));
    assert!(
        got_neg.is_infinite() && got_neg.is_sign_negative(),
        "text={neg_inf_text:?}"
    );

    let nan_text = pg_text(&mut conn, "'NaN'::float8::text");
    let got_nan = parse_f64(&nan_text).unwrap_or_else(|| panic!("parse_f64 NaN: {nan_text:?}"));
    assert!(got_nan.is_nan(), "text={nan_text:?}");
}
