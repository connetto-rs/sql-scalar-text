use sql_values::PgDate;

/// Every accepted spelling of `date`, each a body and the ` BC` suffix that trails the whole
/// value. Year `0000` spells 1 BC without the suffix, as MySQL and SQLite print it.
pub fn date_spellings(date: PgDate) -> Vec<(String, &'static str)> {
    // The infinities have no year, month and day to spell.
    let Some((year, month, day)) = date.ymd() else {
        return Vec::new();
    };
    let month_day = format!("-{month:02}-{day:02}");
    let mut spellings = Vec::with_capacity(3);
    if year > 0 {
        spellings.push((format!("{year:04}{month_day}"), ""));
    }
    if year <= 0 {
        spellings.push((format!("{:04}{month_day}", 1 - year), " BC"));
    }
    if year == 0 {
        spellings.push((format!("0000{month_day}"), ""));
    }
    spellings
}
