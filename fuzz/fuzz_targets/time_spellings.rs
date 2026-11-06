use sql_values::PgTime;

/// Every accepted spelling of `time`: nine fraction digits, six, and the shortest.
pub fn time_spellings(time: PgTime) -> Vec<String> {
    let (hour, minute, second, micro) = time.hms_micro();
    let hms = format!("{hour:02}:{minute:02}:{second:02}");
    let six = format!("{micro:06}");
    let mut spellings = vec![format!("{hms}.{six}000"), format!("{hms}.{six}")];
    match six.trim_end_matches('0') {
        "" => spellings.push(hms),
        shortest => spellings.push(format!("{hms}.{shortest}")),
    }
    spellings
}
