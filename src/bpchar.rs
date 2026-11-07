//! PostgreSQL 18's `bpchar`, the type of `char(n)`, whose equality ignores trailing blanks.

use alloc::borrow::Cow;
use core::{
    fmt,
    hash::{Hash, Hasher},
};

/// A `bpchar` or `char(n)` value, held as written or as a row image carries it, blank-padded to
/// its length.
///
/// Two values are equal as `bpchareq` finds them under a deterministic collation, ignoring
/// trailing blanks (U+0020) on both sides, while a leading blank, a tab or any other whitespace
/// counts. That is the rule `=`, `<>`, `IN` and `IS [NOT] DISTINCT FROM` follow when the other
/// side is a `bpchar`, a `varchar` or an untyped literal. Against `text`, PostgreSQL casts the
/// `bpchar` to `text` instead, which [`PgBpchar::as_text`] models, and compares the result with
/// the `text` exactly.
///
/// ```
/// use sql_values::PgBpchar;
///
/// let row = PgBpchar::new("ab  ");
/// assert_eq!(row, PgBpchar::new("ab"));
/// assert_eq!(row, PgBpchar::new("ab   "));
/// assert_ne!(row, PgBpchar::new(" ab"));
/// assert_ne!(PgBpchar::new("ab\t "), PgBpchar::new("ab"));
/// assert_eq!(PgBpchar::new("    "), PgBpchar::new(""));
///
/// assert_eq!(row.as_text(), "ab");
/// assert_ne!(row.as_text(), "ab ");
/// assert_eq!(row.to_string(), "ab  ");
/// ```
#[derive(Debug, Clone)]
pub struct PgBpchar<'a>(Cow<'a, str>);

impl<'a> PgBpchar<'a> {
    /// The value whose text is `text`, kept as it is, blanks included.
    pub fn new(text: impl Into<Cow<'a, str>>) -> Self {
        Self(text.into())
    }

    /// The text as held, blanks included, as the output function prints it.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The `text` the cast `bpchar::text` produces, without the trailing blanks.
    pub fn as_text(&self) -> &str {
        self.0.trim_end_matches(' ')
    }
}

impl PartialEq<PgBpchar<'_>> for PgBpchar<'_> {
    fn eq(&self, other: &PgBpchar<'_>) -> bool {
        self.as_text() == other.as_text()
    }
}

impl Eq for PgBpchar<'_> {}

impl Hash for PgBpchar<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_text().hash(state);
    }
}

impl fmt::Display for PgBpchar<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
