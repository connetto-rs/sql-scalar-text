# sql-values

[![crates.io](https://img.shields.io/crates/v/sql-values.svg)](https://crates.io/crates/sql-values)
[![docs.rs](https://img.shields.io/docsrs/sql-values)](https://docs.rs/sql-values)
[![CI](https://github.com/connetto-rs/sql-values/actions/workflows/ci.yml/badge.svg)](https://github.com/connetto-rs/sql-values/actions/workflows/ci.yml)
[![Codecov](https://codecov.io/gh/connetto-rs/sql-values/branch/main/graph/badge.svg)](https://codecov.io/gh/connetto-rs/sql-values)
[![MSRV](https://img.shields.io/badge/MSRV-1.88-blue.svg)](https://github.com/connetto-rs/sql-values)
[![license](https://img.shields.io/crates/l/sql-values.svg)](https://github.com/connetto-rs/sql-values/blob/main/LICENSE)

`sql-values` holds the semantics of SQL values. It ports PostgreSQL's own value rules (input, canonical output, order, arithmetic and the explicit cast of a literal), each checked against PostgreSQL 18, and it reads the text forms PostgreSQL, MySQL and SQLite emit for scalar values, so every wire-text consumer decodes the same spelling to the same Rust value. Dates and times are held as PostgreSQL stores them, over its whole range.

```rust
use sql_values::{
    CastTarget, IntervalError, Literal, cast_literal, parse_bool, parse_interval, parse_timestamp,
};

assert_eq!(parse_bool("t"), Some(true));

let span = parse_interval("1.5 days").unwrap();
assert_eq!(span.to_string(), "1 day 12:00:00");
assert_eq!(parse_interval("p1d"), Err(IntervalError::InvalidSyntax));

let at = parse_timestamp("294276-12-31 10:00:00").unwrap();
let day = parse_interval("1 day").unwrap();
assert_eq!(day.subtract_from_timestamp(at).unwrap().to_string(), "294276-12-30 10:00:00");

let level = cast_literal(Literal::Number("2.7"), CastTarget::Integer).unwrap();
assert_eq!(level.to_string(), "3");
```

The crate supports `no_std`. Decimal parsing is available through the `decimal` feature, and `serde` derives for interval, date and time values through the `serde` feature.
