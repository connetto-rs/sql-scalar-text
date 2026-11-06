//! PostgreSQL 18's `DecodeDateTime` and `DecodeTimeOnly`, with `DecodeDate`, `DecodeNumber`,
//! `DecodeNumberField`, `DecodeTime`, `DecodeTimezone` and `ValidateDate`, ported from
//! `datetime.c` with PostgreSQL's names and order of checks. The session state the C reads from globals, `DateOrder`, the
//! session's `TimeZone` and the transaction's start, comes from the [`InputContext`].

use alloc::{borrow::Cow, string::String};

use super::{
    DateTimeError, InputContext, PgTimestampTz, USECS_PER_DAY, USECS_PER_HOUR, USECS_PER_MINUTE,
    USECS_PER_SEC, date2j, days_in_month, j2date, timestamp2tm,
    tokens::{
        ADBC, AMPM, BadFormat, DATE_TOKENS, DAY, DOW, DOY, DTK_DATE_M, DTK_EPOCH, DTK_JULIAN,
        DTK_LATE, DTK_NOW, DTK_TIME, DTK_TIME_M, DTK_TODAY, DTK_TOMORROW, DTK_YESTERDAY, DTK_ZULU,
        DTZ, DTZMOD, DYNTZ, FieldType, IGNORE_DTF, ISOTIME, MONTH, RESERV, TZ, Tokens, UNITS,
        UNKNOWN_FIELD, YEAR, dtk_m, lookup, parse_fraction, parse_fractional_second, strtoint,
        strtoll,
    },
    utc_offset,
};
use crate::{PgTimeZone, zone::Meaning};

/// `HOURS_PER_DAY`.
const HOURS_PER_DAY: i32 = 24;
const AM: i32 = 0;
const PM: i32 = 1;
const HR24: i32 = 2;

/// `DateOrder`, the field order of `DateStyle` for a date written in numbers alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DateOrder {
    /// `MDY`, month, day, year, PostgreSQL's stock order.
    Mdy,
    /// `DMY`, day, month, year.
    Dmy,
    /// `YMD`, year, month, day.
    Ymd,
}

/// A `DTERR` code, or session state the context lacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Dterr {
    /// `DTERR_BAD_FORMAT`.
    BadFormat,
    /// `DTERR_FIELD_OVERFLOW` or `DTERR_MD_FIELD_OVERFLOW`.
    FieldOverflow,
    /// `DTERR_TZDISP_OVERFLOW`.
    TzDispOverflow,
    /// `DTERR_BAD_TIMEZONE` or `DTERR_BAD_ZONE_ABBREV`.
    BadTimezone,
    /// An answer the readers give besides PostgreSQL's codes.
    Other(DateTimeError),
}

impl From<BadFormat> for Dterr {
    fn from(_: BadFormat) -> Self {
        Self::BadFormat
    }
}

impl Dterr {
    /// `DateTimeParseError`'s error for a literal of `pg_type`.
    pub(crate) const fn for_type(self, pg_type: crate::PgType) -> DateTimeError {
        match self {
            Self::BadFormat => DateTimeError::InvalidInputSyntax(pg_type),
            Self::FieldOverflow => DateTimeError::FieldValueOutOfRange,
            Self::TzDispOverflow => DateTimeError::TimeZoneDisplacementOutOfRange,
            Self::BadTimezone => DateTimeError::TimeZoneNotRecognized,
            Self::Other(error) => error,
        }
    }
}

/// `struct pg_tm` as the decoders fill it, with `fsec` beside it.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Tm {
    pub(crate) year: i32,
    pub(crate) mon: i32,
    pub(crate) mday: i32,
    pub(crate) hour: i32,
    pub(crate) min: i32,
    pub(crate) sec: i32,
    pub(crate) yday: i32,
    /// Microseconds of the second, `fsec`.
    pub(crate) fsec: i64,
}

impl Tm {
    /// `time2t`, the clock in microseconds.
    pub(crate) fn clock(&self) -> i64 {
        ((i64::from(self.hour) * 60 + i64::from(self.min)) * 60 + i64::from(self.sec))
            * USECS_PER_SEC
            + self.fsec
    }
}

/// `dtype`, what a literal reads as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DateType {
    /// `DTK_DATE`, the fields of a date and time.
    Date,
    /// `DTK_EPOCH`.
    Epoch,
    /// `DTK_LATE`, `infinity`.
    Late,
    /// `DTK_EARLY`, `-infinity`.
    Early,
}

/// Where the offset of a `DTK_DATE` literal comes from, which `DecodeDateTime` resolves last.
#[derive(Debug, Clone)]
pub(crate) enum ZoneSource<'a> {
    /// `*tzp` set by the literal, seconds west of UTC.
    West(i32),
    /// `namedTz`, `DetermineTimeZoneOffset` in a zone the literal names.
    Named(PgTimeZone),
    /// `abbrevTz`, `DetermineTimeZoneAbbrevOffset` of the uppercased abbreviation.
    Abbreviation {
        zone: Cow<'a, PgTimeZone>,
        upper: String,
    },
    /// The session's `TimeZone`, when the literal writes none.
    Session,
}

/// What `DecodeTimeOnly` leaves its caller.
#[derive(Debug)]
pub(crate) struct DecodedTime<'a> {
    pub(crate) tm: Tm,
    /// Whether the literal writes a full date, which dates the zone's offset in place of the
    /// current date.
    pub(crate) dated: bool,
    /// [`ZoneSource::Named`] only for a dated literal.
    pub(crate) zone: ZoneSource<'a>,
}

/// What `DecodeDateTime` leaves its caller.
#[derive(Debug)]
pub(crate) struct Decoded<'a> {
    pub(crate) dtype: DateType,
    pub(crate) tm: Tm,
    pub(crate) zone: ZoneSource<'a>,
}

/// A `DecodeTimezoneAbbrev` and `DecodeSpecial` answer, the token type and its value.
pub(crate) enum WordType<'a> {
    /// A `datetktbl` token type and value.
    Table(i32, i32),
    /// `TZ` or `DTZ`, an offset west of UTC in seconds.
    Fixed { west: i32, dst: bool },
    /// `DYNTZ`, the uppercased abbreviation and the zone that gives its meaning.
    Dynamic {
        zone: Cow<'a, PgTimeZone>,
        upper: String,
    },
    /// `UNKNOWN_FIELD`.
    Unknown,
}

/// `DecodeTimezoneAbbrev` falling back to `DecodeSpecial`: an abbreviation the session's zone
/// uses, then one of the context's set, then a `datetktbl` word.
pub(crate) fn decode_word<'a>(
    word: &[u8],
    context: InputContext<'a>,
) -> Result<WordType<'a>, Dterr> {
    let upper = String::from_utf8_lossy(word).to_ascii_uppercase();
    if let Some(zone) = context.time_zone
        && let Some((meaning, fixed)) = zone.abbreviation_is_known(&upper)
    {
        return Ok(if fixed {
            WordType::Fixed {
                west: -meaning.offset,
                dst: meaning.dst,
            }
        } else {
            WordType::Dynamic {
                zone: Cow::Borrowed(zone),
                upper,
            }
        });
    }
    match context.abbreviations.lookup(word) {
        Some(Meaning::Fixed { offset, dst }) => {
            return Ok(WordType::Fixed {
                west: -*offset,
                dst: *dst,
            });
        }
        Some(Meaning::Zone(name)) => {
            let zone = PgTimeZone::load(name).ok_or(Dterr::BadTimezone)?;
            return Ok(WordType::Dynamic {
                zone: Cow::Owned(zone),
                upper,
            });
        }
        None => {}
    }
    Ok(match lookup(DATE_TOKENS, word) {
        Some((kind, value)) => WordType::Table(kind, value),
        None => WordType::Unknown,
    })
}

/// `DecodeTimezone`, the offset west of UTC in seconds a numeric zone writes, `*tzp`.
pub(crate) fn decode_timezone(text: &[u8]) -> Result<i32, Dterr> {
    let negative = match text.first() {
        Some(b'+') => false,
        Some(b'-') => true,
        _ => return Err(Dterr::BadFormat),
    };
    let (mut hour, mut at, erange) = strtoint(text, 1);
    if erange {
        return Err(Dterr::TzDispOverflow);
    }
    let mut minute = 0;
    let mut second = 0;
    if text.get(at) == Some(&b':') {
        let (value, end, erange) = strtoint(text, at + 1);
        if erange {
            return Err(Dterr::TzDispOverflow);
        }
        (minute, at) = (value, end);
        if text.get(at) == Some(&b':') {
            let (value, end, erange) = strtoint(text, at + 1);
            if erange {
                return Err(Dterr::TzDispOverflow);
            }
            (second, at) = (value, end);
        }
    } else if at == text.len() && text.len() > 3 {
        minute = hour % 100;
        hour /= 100;
    }
    if !(0..=15).contains(&hour) || !(0..60).contains(&minute) || !(0..60).contains(&second) {
        return Err(Dterr::TzDispOverflow);
    }
    let east = (hour * 60 + minute) * 60 + second;
    if at != text.len() {
        return Err(Dterr::BadFormat);
    }
    Ok(if negative { east } else { -east })
}

/// `DecodeTimeCommon` with `INTERVAL_FULL_RANGE` and `DecodeTime`, a time field into `tm`.
pub(crate) fn decode_time(text: &[u8], tm: &mut Tm) -> Result<(), Dterr> {
    // A time field starts with digits and a colon.
    let (hour, mut at, erange) = strtoll(text, 0);
    if erange {
        return Err(Dterr::FieldOverflow);
    }
    let (mut minute, end, erange) = strtoint(text, at + 1);
    if erange {
        return Err(Dterr::FieldOverflow);
    }
    at = end;
    let (mut hour, mut second, mut fsec) = (hour, 0, 0);
    match text.get(at) {
        None => {}
        Some(b'.') => {
            // `mm:ss.sss`, minutes and seconds.
            fsec = parse_fractional_second(&text[at..])?;
            second = minute;
            minute = i32::try_from(hour).map_err(|_| Dterr::FieldOverflow)?;
            hour = 0;
        }
        // The field holds digits, colons and points alone, so this is a colon.
        Some(_) => {
            let (value, end, erange) = strtoint(text, at + 1);
            if erange {
                return Err(Dterr::FieldOverflow);
            }
            second = value;
            match text.get(end) {
                Some(b'.') => fsec = parse_fractional_second(&text[end..])?,
                None => {}
                Some(_) => return Err(Dterr::BadFormat),
            }
        }
    }
    // The digits make the hour positive and the fraction is within a second.
    if !(0..60).contains(&minute) || !(0..=60).contains(&second) {
        return Err(Dterr::FieldOverflow);
    }
    tm.hour = i32::try_from(hour).map_err(|_| Dterr::FieldOverflow)?;
    tm.min = minute;
    tm.sec = second;
    tm.fsec = i64::from(fsec);
    Ok(())
}

/// `time_overflows` on a clock whose fields are not negative and whose fraction is within a
/// second, as the decoders leave every clock.
pub(crate) fn time_overflows(tm: &Tm) -> bool {
    !(0..=HOURS_PER_DAY).contains(&tm.hour)
        || !(0..60).contains(&tm.min)
        || !(0..=60).contains(&tm.sec)
        || tm.clock() > USECS_PER_DAY
}

/// C's `atoi` of digits, `strtol` saturating and then narrowed to `int`.
fn atoi(digits: &[u8]) -> i32 {
    let (value, _, _) = strtoll(digits, 0);
    // Intended loss, as C's conversion of the `long` to `int` keeps its low 32 bits.
    value as i32
}

/// `DecodeNumberField`, a run of digits as a date or a time, answering `DTK_DATE` or
/// `DTK_TIME` with the fields it sets in `tmask`.
pub(crate) fn decode_number_field(
    text: &[u8],
    fmask: u32,
    tmask: &mut u32,
    tm: &mut Tm,
    is2digits: &mut bool,
) -> Result<i32, Dterr> {
    if !text.iter().all(|b| b.is_ascii_digit() || *b == b'.') {
        return Err(Dterr::BadFormat);
    }
    let mut digits = text;
    if let Some(point) = text.iter().position(|&b| b == b'.') {
        tm.fsec = i64::from(parse_fractional_second(&text[point..])?);
        digits = &text[..point];
    } else if fmask & DTK_DATE_M != DTK_DATE_M && digits.len() >= 6 {
        *tmask = DTK_DATE_M;
        let len = digits.len();
        tm.mday = atoi(&digits[len - 2..]);
        tm.mon = atoi(&digits[len - 4..len - 2]);
        tm.year = atoi(&digits[..len - 4]);
        if len - 4 == 2 {
            *is2digits = true;
        }
        return Ok(super::tokens::DTK_DATE);
    }
    if fmask & DTK_TIME_M != DTK_TIME_M {
        match digits.len() {
            6 => {
                *tmask = DTK_TIME_M;
                tm.sec = atoi(&digits[4..]);
                tm.min = atoi(&digits[2..4]);
                tm.hour = atoi(&digits[..2]);
                return Ok(DTK_TIME);
            }
            4 => {
                *tmask = DTK_TIME_M;
                tm.sec = 0;
                tm.min = atoi(&digits[2..]);
                tm.hour = atoi(&digits[..2]);
                return Ok(DTK_TIME);
            }
            _ => {}
        }
    }
    Err(Dterr::BadFormat)
}

/// `DecodeNumber`, one number of a date, placed by what the literal has so far and the
/// context's `DateOrder`.
pub(crate) fn decode_number(
    text: &[u8],
    have_text_month: bool,
    fmask: u32,
    tmask: &mut u32,
    tm: &mut Tm,
    is2digits: &mut bool,
    order: Option<DateOrder>,
) -> Result<(), Dterr> {
    *tmask = 0;
    let flen = text.len();
    let (value, end, erange) = strtoint(text, 0);
    if erange {
        return Err(Dterr::FieldOverflow);
    }
    if end == 0 {
        return Err(Dterr::BadFormat);
    }
    // The callers pass digits, with a fraction only after one or two of them.
    if end < flen {
        tm.fsec = i64::from(parse_fractional_second(&text[end..])?);
    }
    if flen == 3 && fmask & DTK_DATE_M == dtk_m(YEAR) && (1..=366).contains(&value) {
        *tmask = dtk_m(DOY) | dtk_m(MONTH) | dtk_m(DAY);
        tm.yday = value;
        return Ok(());
    }
    let needs_order = || order.ok_or(Dterr::Other(DateTimeError::NeedsDateOrder));
    match fmask & DTK_DATE_M {
        0 => {
            if flen >= 3 || needs_order()? == DateOrder::Ymd {
                *tmask = dtk_m(YEAR);
                tm.year = value;
            } else if needs_order()? == DateOrder::Dmy {
                *tmask = dtk_m(DAY);
                tm.mday = value;
            } else {
                *tmask = dtk_m(MONTH);
                tm.mon = value;
            }
        }
        mask if mask == dtk_m(YEAR) => {
            *tmask = dtk_m(MONTH);
            tm.mon = value;
        }
        mask if mask == dtk_m(MONTH) => {
            if have_text_month && (flen >= 3 || needs_order()? == DateOrder::Ymd) {
                *tmask = dtk_m(YEAR);
                tm.year = value;
            } else {
                *tmask = dtk_m(DAY);
                tm.mday = value;
            }
        }
        mask if mask == dtk_m(YEAR) | dtk_m(MONTH) => {
            if have_text_month && flen >= 3 && *is2digits {
                *tmask = dtk_m(DAY);
                tm.mday = tm.year;
                tm.year = value;
                *is2digits = false;
            } else {
                *tmask = dtk_m(DAY);
                tm.mday = value;
            }
        }
        mask if mask == dtk_m(DAY) => {
            *tmask = dtk_m(MONTH);
            tm.mon = value;
        }
        mask if mask == dtk_m(MONTH) | dtk_m(DAY) => {
            *tmask = dtk_m(YEAR);
            tm.year = value;
        }
        // The full date. A day without a month comes first and only under `DMY`, so a year
        // and a day never stand alone.
        _ => {
            decode_number_field(text, fmask, tmask, tm, is2digits)?;
            return Ok(());
        }
    }
    if *tmask == dtk_m(YEAR) {
        *is2digits = flen <= 2;
    }
    Ok(())
}

/// `DecodeDate`, a date of delimited fields, text months first.
pub(crate) fn decode_date(
    text: &[u8],
    mut fmask: u32,
    tmask: &mut u32,
    is2digits: &mut bool,
    tm: &mut Tm,
    order: Option<DateOrder>,
) -> Result<(), Dterr> {
    *tmask = 0;
    let mut fields: [&[u8]; super::tokens::MAX_DATE_FIELDS] = [&[]; super::tokens::MAX_DATE_FIELDS];
    let mut count = 0;
    let mut at = 0;
    while at < text.len() && count < fields.len() {
        while at < text.len() && !text[at].is_ascii_alphanumeric() {
            at += 1;
        }
        if at == text.len() {
            return Err(Dterr::BadFormat);
        }
        let start = at;
        if text[at].is_ascii_digit() {
            while at < text.len() && text[at].is_ascii_digit() {
                at += 1;
            }
        } else {
            while at < text.len() && text[at].is_ascii_alphabetic() {
                at += 1;
            }
        }
        fields[count] = &text[start..at];
        count += 1;
        // The C overwrites the character after the field with its terminator.
        at += usize::from(at < text.len());
    }
    let fields = &fields[..count];
    let mut have_text_month = false;
    let mut done = [false; super::tokens::MAX_DATE_FIELDS];
    for (field, done) in fields.iter().zip(&mut done) {
        if field.first().is_some_and(u8::is_ascii_alphabetic) {
            let (kind, value) = lookup(DATE_TOKENS, field).unwrap_or((UNKNOWN_FIELD, 0));
            if kind == IGNORE_DTF {
                continue;
            }
            let dmask = dtk_m(kind);
            if kind != MONTH {
                return Err(Dterr::BadFormat);
            }
            tm.mon = value;
            have_text_month = true;
            if fmask & dmask != 0 {
                return Err(Dterr::BadFormat);
            }
            fmask |= dmask;
            *tmask |= dmask;
            *done = true;
        }
    }
    for (field, done) in fields.iter().zip(done) {
        if done {
            continue;
        }
        let mut dmask = 0;
        decode_number(
            field,
            have_text_month,
            fmask,
            &mut dmask,
            tm,
            is2digits,
            order,
        )?;
        // `DecodeNumber` sets only fields not yet set.
        fmask |= dmask;
        *tmask |= dmask;
    }
    if fmask & !(dtk_m(DOY) | dtk_m(TZ)) != DTK_DATE_M {
        return Err(Dterr::BadFormat);
    }
    Ok(())
}

/// `ValidateDate`, the year by era and digits, the day of year, and the month and day.
pub(crate) fn validate_date(
    fmask: u32,
    isjulian: bool,
    is2digits: bool,
    bc: bool,
    tm: &mut Tm,
) -> Result<(), Dterr> {
    if fmask & dtk_m(YEAR) != 0 {
        // A Julian day's year is already right.
        if isjulian {
            // Nothing to adjust.
        } else if bc {
            if tm.year <= 0 {
                return Err(Dterr::FieldOverflow);
            }
            tm.year = -(tm.year - 1);
        } else if is2digits {
            // Two digits make a year from 0 to 99.
            tm.year += if tm.year < 70 { 2000 } else { 1900 };
        } else if tm.year <= 0 {
            return Err(Dterr::FieldOverflow);
        }
    }
    if fmask & dtk_m(DOY) != 0 {
        (tm.year, tm.mon, tm.mday) = j2date(date2j(tm.year, 1, 1).wrapping_add(tm.yday - 1));
    }
    if fmask & dtk_m(MONTH) != 0 && !(1..=12).contains(&tm.mon) {
        return Err(Dterr::FieldOverflow);
    }
    if fmask & dtk_m(DAY) != 0 && !(1..=31).contains(&tm.mday) {
        return Err(Dterr::FieldOverflow);
    }
    if fmask & DTK_DATE_M == DTK_DATE_M && tm.mday > days_in_month(tm.year, tm.mon) {
        return Err(Dterr::FieldOverflow);
    }
    Ok(())
}

/// `GetCurrentTimeUsec`, the local fields of the transaction's start in the session's zone and
/// its offset west of UTC.
pub(crate) fn current_time(context: InputContext<'_>) -> Result<(Tm, i32), Dterr> {
    let now = context
        .now
        .ok_or(Dterr::Other(DateTimeError::NeedsCurrentTime))?;
    let zone = context
        .time_zone
        .ok_or(Dterr::Other(DateTimeError::NeedsTimeZone))?;
    local_fields(now, zone).map_err(Dterr::Other)
}

/// `timestamp2tm` in `zone`, the local fields of an instant and its offset west of UTC, which
/// refuses an infinity as out of range.
pub(crate) fn local_fields(
    at: PgTimestampTz,
    zone: &PgTimeZone,
) -> Result<(Tm, i32), DateTimeError> {
    if !at.is_finite() {
        return Err(DateTimeError::TimestampOutOfRange);
    }
    let offset = utc_offset(zone, at.pg_micros())?;
    let (year, mon, mday, clock) = timestamp2tm(at.pg_micros() + i64::from(offset) * USECS_PER_SEC);
    let tm = Tm {
        year,
        mon,
        mday,
        hour: i32::try_from(clock / USECS_PER_HOUR).expect("an hour of the day fits int"),
        min: i32::try_from(clock % USECS_PER_HOUR / USECS_PER_MINUTE).expect("a minute fits int"),
        sec: i32::try_from(clock % USECS_PER_MINUTE / USECS_PER_SEC).expect("a second fits int"),
        yday: 0,
        fsec: clock % USECS_PER_SEC,
    };
    Ok((tm, -offset))
}

/// A Julian day number after `j` or `julian`, with the fraction of the day after it, into `tm`,
/// answering the fields it sets.
fn julian_day(value: i32, fraction: &[u8], tm: &mut Tm) -> Result<u32, Dterr> {
    let mut tmask = DTK_DATE_M;
    (tm.year, tm.mon, tm.mday) = j2date(value);
    if !fraction.is_empty() {
        let fraction = parse_fraction(fraction)?;
        // C's conversion of the `double` to `int64` truncates.
        let time =
            crate::float::truncate_to_i64(fraction * crate::float::int_to_f64(USECS_PER_DAY));
        dt2time(time, tm);
        tmask |= DTK_TIME_M;
    }
    Ok(tmask)
}

/// The `AM` or `PM` a literal writes, applied to its hour.
fn apply_meridian(mer: i32, tm: &mut Tm) -> Result<(), Dterr> {
    if mer != HR24 && tm.hour > HOURS_PER_DAY / 2 {
        return Err(Dterr::FieldOverflow);
    }
    if mer == AM && tm.hour == HOURS_PER_DAY / 2 {
        tm.hour = 0;
    } else if mer == PM && tm.hour != HOURS_PER_DAY / 2 {
        tm.hour += HOURS_PER_DAY / 2;
    }
    Ok(())
}

/// `dt2time`, a clock in microseconds as hours, minutes, seconds and the fraction.
fn dt2time(time: i64, tm: &mut Tm) {
    let narrow = |value: i64| i32::try_from(value).expect("a Julian day's clock fits int");
    tm.hour = narrow(time / USECS_PER_HOUR);
    let time = time - i64::from(tm.hour) * USECS_PER_HOUR;
    tm.min = narrow(time / USECS_PER_MINUTE);
    let time = time - i64::from(tm.min) * USECS_PER_MINUTE;
    tm.sec = narrow(time / USECS_PER_SEC);
    tm.fsec = time - i64::from(tm.sec) * USECS_PER_SEC;
}

/// `DecodeDateTime` over the tokens of a date or timestamp literal.
#[expect(
    clippy::too_many_lines,
    reason = "the port keeps `DecodeDateTime` whole, one arm per token type, as PostgreSQL does"
)]
pub(crate) fn decode_date_time<'a>(
    tokens: &Tokens,
    context: InputContext<'a>,
) -> Result<Decoded<'a>, Dterr> {
    let mut fmask: u32 = 0;
    let mut ptype = 0;
    let mut mer = HR24;
    let mut have_text_month = false;
    let mut isjulian = false;
    let mut is2digits = false;
    let mut bc = false;
    let mut dtype = DateType::Date;
    let mut tm = Tm::default();
    let mut tz: i32 = 0;
    let mut named: Option<PgTimeZone> = None;
    let mut abbreviated: Option<(Cow<'a, PgTimeZone>, String)> = None;
    let order = context.date_order;

    for index in 0..tokens.len() {
        let (field, kind) = tokens.field(index);
        let mut tmask: u32;
        match kind {
            FieldType::Date => {
                if ptype == DTK_JULIAN {
                    let (jday, end, erange) = strtoint(field, 0);
                    if erange {
                        return Err(Dterr::FieldOverflow);
                    }
                    (tm.year, tm.mon, tm.mday) = j2date(jday);
                    isjulian = true;
                    tz = decode_timezone(&field[end..])?;
                    tmask = DTK_DATE_M | DTK_TIME_M | dtk_m(TZ);
                    ptype = 0;
                } else if ptype != 0
                    || fmask & (dtk_m(MONTH) | dtk_m(DAY)) == dtk_m(MONTH) | dtk_m(DAY)
                {
                    if field[0].is_ascii_digit() || ptype != 0 {
                        if ptype != 0 {
                            if ptype != DTK_TIME {
                                return Err(Dterr::BadFormat);
                            }
                            ptype = 0;
                        }
                        if fmask & DTK_TIME_M == DTK_TIME_M {
                            return Err(Dterr::BadFormat);
                        }
                        let dash = field
                            .iter()
                            .position(|&b| b == b'-')
                            .ok_or(Dterr::BadFormat)?;
                        tz = decode_timezone(&field[dash..])?;
                        tmask = 0;
                        decode_number_field(
                            &field[..dash],
                            fmask,
                            &mut tmask,
                            &mut tm,
                            &mut is2digits,
                        )?;
                        tmask |= dtk_m(TZ);
                    } else {
                        let name = String::from_utf8_lossy(field);
                        named = Some(PgTimeZone::load(&name).ok_or(Dterr::BadTimezone)?);
                        tmask = dtk_m(TZ);
                    }
                } else {
                    tmask = 0;
                    decode_date(field, fmask, &mut tmask, &mut is2digits, &mut tm, order)?;
                }
            }
            FieldType::Time => {
                if ptype != 0 {
                    if ptype != DTK_TIME {
                        return Err(Dterr::BadFormat);
                    }
                    ptype = 0;
                }
                decode_time(field, &mut tm)?;
                tmask = DTK_TIME_M;
                if time_overflows(&tm) {
                    return Err(Dterr::FieldOverflow);
                }
            }
            FieldType::Tz => {
                tz = decode_timezone(field)?;
                tmask = dtk_m(TZ);
            }
            FieldType::Number => {
                if ptype != 0 {
                    // A number field is digits with an optional fraction.
                    let (value, end, erange) = strtoint(field, 0);
                    if erange {
                        return Err(Dterr::FieldOverflow);
                    }
                    match ptype {
                        DTK_JULIAN => {
                            tmask = julian_day(value, &field[end..], &mut tm)?;
                            isjulian = true;
                        }
                        DTK_TIME => {
                            tmask = 0;
                            // With the date marked set, a success reads a time.
                            decode_number_field(
                                field,
                                fmask | DTK_DATE_M,
                                &mut tmask,
                                &mut tm,
                                &mut is2digits,
                            )?;
                        }
                        _ => return Err(Dterr::BadFormat),
                    }
                    ptype = 0;
                    dtype = DateType::Date;
                } else {
                    let point = field.iter().position(|&b| b == b'.');
                    tmask = 0;
                    if point.is_some() && fmask & DTK_DATE_M == 0 {
                        decode_date(field, fmask, &mut tmask, &mut is2digits, &mut tm, order)?;
                    } else if point.is_some_and(|point| point > 2)
                        || (field.len() >= 6
                            && (fmask & DTK_DATE_M == 0 || fmask & DTK_TIME_M == 0))
                    {
                        // A run-together date or time, with or without a fraction.
                        decode_number_field(field, fmask, &mut tmask, &mut tm, &mut is2digits)?;
                    } else {
                        decode_number(
                            field,
                            have_text_month,
                            fmask,
                            &mut tmask,
                            &mut tm,
                            &mut is2digits,
                            order,
                        )?;
                    }
                }
            }
            FieldType::String | FieldType::Special => {
                let word = decode_word(field, context)?;
                let (kind, value) = match word {
                    WordType::Fixed { west, dst } => {
                        tz = west;
                        (if dst { DTZ } else { TZ }, 0)
                    }
                    WordType::Dynamic { zone, upper } => {
                        abbreviated = Some((zone, upper));
                        (DYNTZ, 0)
                    }
                    WordType::Table(kind, value) => (kind, value),
                    WordType::Unknown => (UNKNOWN_FIELD, 0),
                };
                if kind == IGNORE_DTF {
                    continue;
                }
                tmask = dtk_m(kind);
                match kind {
                    RESERV => match value {
                        DTK_NOW => {
                            tmask = DTK_DATE_M | DTK_TIME_M | dtk_m(TZ);
                            dtype = DateType::Date;
                            (tm, tz) = current_time(context)?;
                        }
                        DTK_YESTERDAY | DTK_TODAY | DTK_TOMORROW => {
                            tmask = DTK_DATE_M;
                            dtype = DateType::Date;
                            let (now, _) = current_time(context)?;
                            let shift = match value {
                                DTK_YESTERDAY => -1,
                                DTK_TOMORROW => 1,
                                _ => 0,
                            };
                            (tm.year, tm.mon, tm.mday) =
                                j2date(date2j(now.year, now.mon, now.mday) + shift);
                        }
                        DTK_ZULU => {
                            tmask = DTK_TIME_M | dtk_m(TZ);
                            dtype = DateType::Date;
                            (tm.hour, tm.min, tm.sec) = (0, 0, 0);
                            tz = 0;
                        }
                        // `epoch`, `infinity` and `-infinity`.
                        _ => {
                            tmask = DTK_DATE_M | DTK_TIME_M | dtk_m(TZ);
                            dtype = match value {
                                DTK_EPOCH => DateType::Epoch,
                                DTK_LATE => DateType::Late,
                                _ => DateType::Early,
                            };
                        }
                    },
                    MONTH => {
                        if fmask & dtk_m(MONTH) != 0
                            && !have_text_month
                            && fmask & dtk_m(DAY) == 0
                            && (1..=31).contains(&tm.mon)
                        {
                            tm.mday = tm.mon;
                            tmask = dtk_m(DAY);
                        }
                        have_text_month = true;
                        tm.mon = value;
                    }
                    DTZMOD => {
                        tmask |= dtk_m(DTZ);
                        tz -= value;
                    }
                    DTZ => tmask |= dtk_m(TZ),
                    TZ => {}
                    DYNTZ => tmask |= dtk_m(TZ),
                    AMPM => mer = value,
                    ADBC => bc = value == 1,
                    DOW => {}
                    UNITS => {
                        tmask = 0;
                        if ptype != 0 {
                            return Err(Dterr::BadFormat);
                        }
                        ptype = value;
                    }
                    ISOTIME => {
                        tmask = 0;
                        if fmask & DTK_DATE_M != DTK_DATE_M || ptype != 0 {
                            return Err(Dterr::BadFormat);
                        }
                        ptype = value;
                    }
                    // `UNKNOWN_FIELD`, the one kind left.
                    _ => {
                        let name = String::from_utf8_lossy(field);
                        named = Some(PgTimeZone::load(&name).ok_or(Dterr::BadFormat)?);
                        tmask = dtk_m(TZ);
                    }
                }
            }
        }
        if tmask & fmask != 0 {
            return Err(Dterr::BadFormat);
        }
        fmask |= tmask;
    }
    if ptype != 0 {
        return Err(Dterr::BadFormat);
    }

    let mut zone = ZoneSource::West(tz);
    if dtype == DateType::Date {
        validate_date(fmask, isjulian, is2digits, bc, &mut tm)?;
        apply_meridian(mer, &mut tm)?;
        if fmask & DTK_DATE_M != DTK_DATE_M {
            return Err(Dterr::BadFormat);
        }
        let modified = fmask & dtk_m(DTZMOD) != 0;
        if let Some(named) = named {
            if modified {
                return Err(Dterr::BadFormat);
            }
            zone = ZoneSource::Named(named);
        }
        if let Some((abbreviation_zone, upper)) = abbreviated {
            if modified {
                return Err(Dterr::BadFormat);
            }
            zone = ZoneSource::Abbreviation {
                zone: abbreviation_zone,
                upper,
            };
        }
        if fmask & dtk_m(TZ) == 0 {
            if modified {
                return Err(Dterr::BadFormat);
            }
            zone = ZoneSource::Session;
        }
    }
    Ok(Decoded { dtype, tm, zone })
}

/// `DecodeTimeOnly` over the tokens of a `time` or `timetz` literal, with `tzp` given as
/// `time_in` and `timetz_in` give it.
#[expect(
    clippy::too_many_lines,
    reason = "the port keeps `DecodeTimeOnly` whole, one arm per token type, as PostgreSQL does"
)]
pub(crate) fn decode_time_only<'a>(
    tokens: &Tokens,
    context: InputContext<'a>,
) -> Result<DecodedTime<'a>, Dterr> {
    let mut fmask: u32 = 0;
    let mut ptype = 0;
    let mut mer = HR24;
    let mut isjulian = false;
    let mut is2digits = false;
    let mut bc = false;
    let mut tm = Tm::default();
    let mut tz: i32 = 0;
    let mut named: Option<PgTimeZone> = None;
    let mut abbreviated: Option<(Cow<'a, PgTimeZone>, String)> = None;
    let order = context.date_order;
    let count = tokens.len();
    // A date is read only as the first field, before a time or with a date last.
    let last_is_date = count >= 2 && tokens.field(count - 1).1 == FieldType::Date;
    let second_is_time = count >= 2 && tokens.field(1).1 == FieldType::Time;

    for index in 0..count {
        let (field, kind) = tokens.field(index);
        let mut tmask: u32;
        match kind {
            FieldType::Date => {
                if index == 0 && (last_is_date || second_is_time) {
                    tmask = 0;
                    decode_date(field, fmask, &mut tmask, &mut is2digits, &mut tm, order)?;
                } else if field[0].is_ascii_digit() {
                    if fmask & DTK_TIME_M == DTK_TIME_M {
                        return Err(Dterr::BadFormat);
                    }
                    let dash = field
                        .iter()
                        .position(|&b| b == b'-')
                        .ok_or(Dterr::BadFormat)?;
                    tz = decode_timezone(&field[dash..])?;
                    tmask = 0;
                    decode_number_field(
                        &field[..dash],
                        fmask | DTK_DATE_M,
                        &mut tmask,
                        &mut tm,
                        &mut is2digits,
                    )?;
                    tmask |= dtk_m(TZ);
                } else {
                    let name = String::from_utf8_lossy(field);
                    named = Some(PgTimeZone::load(&name).ok_or(Dterr::BadTimezone)?);
                    tmask = dtk_m(TZ);
                }
            }
            FieldType::Time => {
                if ptype != 0 {
                    if ptype != DTK_TIME {
                        return Err(Dterr::BadFormat);
                    }
                    ptype = 0;
                }
                decode_time(field, &mut tm)?;
                tmask = DTK_TIME_M;
            }
            FieldType::Tz => {
                tz = decode_timezone(field)?;
                tmask = dtk_m(TZ);
            }
            FieldType::Number => {
                if ptype != 0 {
                    // A number field is digits with an optional fraction.
                    let (value, end, erange) = strtoint(field, 0);
                    if erange {
                        return Err(Dterr::FieldOverflow);
                    }
                    match ptype {
                        DTK_JULIAN => {
                            tmask = julian_day(value, &field[end..], &mut tm)?;
                            isjulian = true;
                        }
                        DTK_TIME => {
                            tmask = 0;
                            // With the date marked set, a success reads a time.
                            decode_number_field(
                                field,
                                fmask | DTK_DATE_M,
                                &mut tmask,
                                &mut tm,
                                &mut is2digits,
                            )?;
                        }
                        _ => return Err(Dterr::BadFormat),
                    }
                    ptype = 0;
                } else {
                    tmask = 0;
                    match field.iter().position(|&b| b == b'.') {
                        Some(_) if index == 0 && last_is_date => {
                            decode_date(field, fmask, &mut tmask, &mut is2digits, &mut tm, order)?;
                        }
                        Some(point) if point <= 2 => return Err(Dterr::BadFormat),
                        Some(_) => {
                            decode_number_field(
                                field,
                                fmask | DTK_DATE_M,
                                &mut tmask,
                                &mut tm,
                                &mut is2digits,
                            )?;
                        }
                        None if field.len() > 4 => {
                            decode_number_field(
                                field,
                                fmask | DTK_DATE_M,
                                &mut tmask,
                                &mut tm,
                                &mut is2digits,
                            )?;
                        }
                        None => decode_number(
                            field,
                            false,
                            fmask | DTK_DATE_M,
                            &mut tmask,
                            &mut tm,
                            &mut is2digits,
                            order,
                        )?,
                    }
                }
            }
            FieldType::String | FieldType::Special => {
                let (kind, value) = match decode_word(field, context)? {
                    WordType::Fixed { west, dst } => {
                        tz = west;
                        (if dst { DTZ } else { TZ }, 0)
                    }
                    WordType::Dynamic { zone, upper } => {
                        abbreviated = Some((zone, upper));
                        (DYNTZ, 0)
                    }
                    WordType::Table(kind, value) => (kind, value),
                    WordType::Unknown => (UNKNOWN_FIELD, 0),
                };
                if kind == IGNORE_DTF {
                    continue;
                }
                tmask = dtk_m(kind);
                match kind {
                    RESERV => match value {
                        DTK_NOW => {
                            tmask = DTK_TIME_M;
                            (tm, _) = current_time(context)?;
                        }
                        DTK_ZULU => {
                            tmask = DTK_TIME_M | dtk_m(TZ);
                            (tm.hour, tm.min, tm.sec) = (0, 0, 0);
                        }
                        _ => return Err(Dterr::BadFormat),
                    },
                    DTZMOD => {
                        tmask |= dtk_m(DTZ);
                        tz -= value;
                    }
                    DTZ | DYNTZ => tmask |= dtk_m(TZ),
                    TZ => {}
                    AMPM => mer = value,
                    ADBC => bc = value == 1,
                    UNITS | ISOTIME => {
                        tmask = 0;
                        if ptype != 0 {
                            return Err(Dterr::BadFormat);
                        }
                        ptype = value;
                    }
                    UNKNOWN_FIELD => {
                        let name = String::from_utf8_lossy(field);
                        named = Some(PgTimeZone::load(&name).ok_or(Dterr::BadFormat)?);
                        tmask = dtk_m(TZ);
                    }
                    // `MONTH` and `DOW`.
                    _ => return Err(Dterr::BadFormat),
                }
            }
        }
        if tmask & fmask != 0 {
            return Err(Dterr::BadFormat);
        }
        fmask |= tmask;
    }
    if ptype != 0 {
        return Err(Dterr::BadFormat);
    }

    validate_date(fmask, isjulian, is2digits, bc, &mut tm)?;
    apply_meridian(mer, &mut tm)?;
    if time_overflows(&tm) {
        return Err(Dterr::FieldOverflow);
    }
    if fmask & DTK_TIME_M != DTK_TIME_M {
        return Err(Dterr::BadFormat);
    }
    let modified = fmask & dtk_m(DTZMOD) != 0;
    // `DecodeDate` and a Julian day set the date whole, so a literal has all of it or none.
    let dated = fmask & DTK_DATE_M == DTK_DATE_M;
    let mut zone = ZoneSource::West(tz);
    if let Some(named) = named {
        if modified {
            return Err(Dterr::BadFormat);
        }
        zone = match named.fixed_offset() {
            Some(east) => ZoneSource::West(-east),
            None if dated => ZoneSource::Named(named),
            None => return Err(Dterr::BadFormat),
        };
    }
    if let Some((abbreviation_zone, upper)) = abbreviated {
        if modified {
            return Err(Dterr::BadFormat);
        }
        zone = ZoneSource::Abbreviation {
            zone: abbreviation_zone,
            upper,
        };
    }
    if fmask & dtk_m(TZ) == 0 {
        if modified {
            return Err(Dterr::BadFormat);
        }
        zone = ZoneSource::Session;
    }
    Ok(DecodedTime { tm, dated, zone })
}
