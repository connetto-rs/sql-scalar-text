# sql-scalar-text

[![crates.io](https://img.shields.io/crates/v/sql-scalar-text.svg)](https://crates.io/crates/sql-scalar-text)
[![docs.rs](https://img.shields.io/docsrs/sql-scalar-text)](https://docs.rs/sql-scalar-text)
[![CI](https://github.com/connetto-rs/sql-scalar-text/actions/workflows/ci.yml/badge.svg)](https://github.com/connetto-rs/sql-scalar-text/actions/workflows/ci.yml)
[![MSRV](https://img.shields.io/badge/MSRV-1.88-blue.svg)](https://github.com/connetto-rs/sql-scalar-text)
[![license](https://img.shields.io/crates/l/sql-scalar-text.svg)](https://github.com/connetto-rs/sql-scalar-text/blob/main/LICENSE)

`sql-scalar-text` parses the text forms that PostgreSQL, MySQL, and SQLite emit for scalar values. Its shared acceptance set lets every wire-text consumer decode the same spelling to the same Rust value.

```rust
assert_eq!(sql_scalar_text::parse_bool("t"), Some(true));
```

The crate supports `no_std`. Decimal parsing is available through the `decimal` feature.
