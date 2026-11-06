//! `timezone_abbreviations` files read as PostgreSQL 18.6 reads them, each refusal's message and
//! each accepted file's meanings measured on the engine with the file installed in its
//! `timezonesets` directory.

use sql_values::{InputContext, PgTimestampTz, TimeZoneAbbreviations, TimeZoneAbbreviationsError};

/// The files the cases install, with the bundled ones they include.
fn file(name: &str) -> Option<&'static str> {
    Some(match name {
        "Good" => concat!(
            "@INCLUDE Default\n@OVERRIDE\nIST 19800 # India\nFOO   3600 D\nBAR Europe/Rome\n",
            "PLUS +7200\nBAZ Etc/GMT-3\nLMT Europe/Rome\nXXX XXX3YYY\nQUX XXX3YYY\n",
        ),
        "Sign" => "FOO +\n",
        "Clash" => "@INCLUDE Default\nIST 19800\n",
        "Same" => "@INCLUDE Default\nIST 7200\nEST -18000\n",
        "Long" => "ABCDEFGHIJK 3600\n",
        "Range" => "FOO 50401\n",
        "Neg" => "FOO -50400\n",
        "Bad" => "FOO 36x0\n",
        "Syntax" => "FOO 3600 D extra\n",
        "Missing" => "FOO\n",
        "Inc" => "@INCLUDE\n",
        "Nope" => "@INCLUDE Nowhere\n",
        "Loop" => "@INCLUDE Loop\n",
        "Badzone" => "QQQ Nowhere/Zone\n",
        "Wrap" => "FOO 4294967296\n",
        "Big" => "FOO 99999999999999999999\n",
        "Weird" => "@includeDefault\n",
        "Name" => "@INCLUDE Def1\n",
        other => TimeZoneAbbreviations::bundled_file(other)?,
    })
}

fn load(name: &str) -> Result<TimeZoneAbbreviations, TimeZoneAbbreviationsError> {
    TimeZoneAbbreviations::load(name, file)
}

/// `literal` read as a `timestamptz` under the set `name`, printed at UTC.
fn read(name: &str, literal: &str) -> String {
    let set = load(name).unwrap_or_else(|error| panic!("{name}: {error}"));
    let context = InputContext::new().with_time_zone_abbreviations(&set);
    match PgTimestampTz::parse(literal, context) {
        Ok(at) => at.to_string(),
        Err(error) => format!("ERROR: {error}"),
    }
}

#[test]
fn files_postgresql_refuses_name_its_message() {
    let message = |name| load(name).map(|_| ()).unwrap_err().to_string();
    assert_eq!(
        message("Clash"),
        "time zone abbreviation \"ist\" is multiply defined"
    );
    assert_eq!(
        message("Long"),
        "time zone abbreviation \"ABCDEFGHIJK\" is too long (maximum 10 characters) in time zone file \"Long\", line 1"
    );
    assert_eq!(
        message("Range"),
        "time zone offset 50401 is out of range in time zone file \"Range\", line 1"
    );
    assert_eq!(
        message("Bad"),
        "invalid number for time zone offset in time zone file \"Bad\", line 1"
    );
    assert_eq!(
        message("Syntax"),
        "invalid syntax in time zone file \"Syntax\", line 1"
    );
    assert_eq!(
        message("Missing"),
        "missing time zone offset in time zone file \"Missing\", line 1"
    );
    assert_eq!(
        message("Inc"),
        "@INCLUDE without file name in time zone file \"Inc\", line 1"
    );
    assert_eq!(message("Nope"), "could not read time zone file \"Nowhere\"");
    assert_eq!(
        message("Loop"),
        "time zone file recursion limit exceeded in file \"Loop\""
    );
    assert_eq!(message("Name"), "invalid time zone file name \"Def1\"");
    assert_eq!(
        message("Sign"),
        "invalid number for time zone offset in time zone file \"Sign\", line 1"
    );
    let long_line = format!("X{:01030} 3600\n", 0);
    let refused = TimeZoneAbbreviations::load("Longline", |name| {
        (name == "Longline").then_some(long_line.as_str())
    });
    assert_eq!(
        refused.map(|_| ()).unwrap_err().to_string(),
        "line is too long in time zone file \"Longline\", line 1"
    );
    assert!(matches!(
        load("Clash"),
        Err(TimeZoneAbbreviationsError::MultiplyDefined { line: 2, .. })
    ));
}

#[test]
fn files_postgresql_accepts_read_as_it_reads_them() {
    assert_eq!(
        read("Good", "2026-01-15 12:00 foo"),
        "2026-01-15 11:00:00+00"
    );
    assert_eq!(
        read("Good", "2026-01-15 12:00 ist"),
        "2026-01-15 06:30:00+00"
    );
    assert_eq!(
        read("Good", "2026-01-15 12:00 bar"),
        "2026-01-15 11:00:00+00"
    );
    assert_eq!(
        read("Good", "2026-01-15 12:00 plus"),
        "2026-01-15 10:00:00+00"
    );
    // A zone of one offset, and one whose earliest abbreviation predates its first change.
    assert_eq!(
        read("Good", "2026-01-15 12:00 baz"),
        "2026-01-15 09:00:00+00"
    );
    assert_eq!(
        read("Good", "1950-01-01 12:00 lmt"),
        "1950-01-01 11:10:04+00"
    );
    // POSIX rules, under the standard abbreviation and under one the zone lacks.
    assert_eq!(
        read("Good", "2026-07-15 12:00 xxx"),
        "2026-07-15 15:00:00+00"
    );
    assert_eq!(
        read("Good", "2026-07-15 12:00 qux"),
        "2026-07-15 14:00:00+00"
    );
    assert_eq!(
        read("Same", "2026-01-15 12:00 ist"),
        "2026-01-15 10:00:00+00"
    );
    assert_eq!(
        read("Neg", "2026-01-15 12:00 foo"),
        "2026-01-16 02:00:00+00"
    );
    // C's `int` keeps the low 32 bits of the `long` `strtol` saturates at.
    assert_eq!(
        read("Wrap", "2026-01-15 12:00 foo"),
        "2026-01-15 12:00:00+00"
    );
    assert_eq!(
        read("Big", "2026-01-15 12:00 foo"),
        "2026-01-15 12:00:01+00"
    );
    assert_eq!(
        read("Weird", "2026-01-15 12:00 cest"),
        "2026-01-15 10:00:00+00"
    );
    // A zone that does not exist is refused when a literal writes its abbreviation.
    assert_eq!(
        read("Badzone", "2026-01-15 12:00 qqq"),
        "ERROR: time zone not recognized"
    );
}

#[test]
fn bundled_sets_are_postgresql_s() {
    for name in ["Default", "Australia", "India"] {
        let bundled = TimeZoneAbbreviations::bundled(name).expect("a bundled set");
        assert_eq!(load(name).as_ref(), Ok(bundled), "{name}");
    }
    assert!(TimeZoneAbbreviations::bundled("default").is_none());
    assert!(TimeZoneAbbreviations::bundled_file("Good").is_none());
}
