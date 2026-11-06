//! PostgreSQL 18's `TimeZone` setting as `check_timezone`, `pg_tzset` and `tzparse` read it, and the
//! offsets `pg_localtime` and `DetermineTimeZoneOffset` find in a zone.
//!
//! IANA zones come from jiff's bundled database, which is asked only for a zone's offset at an
//! instant and for its next transition. Fixed offsets and POSIX rules are ported and hold over
//! PostgreSQL's whole range.

use alloc::string::{String, ToString};
use core::{
    fmt,
    hash::{Hash, Hasher},
};

use jiff::{Timestamp, tz::TimeZone};

use crate::{
    IntervalError,
    datetime::{date2j, days_in_month, j2date},
    interval::strtod,
    parse_interval,
};

const SECS_PER_MINUTE: i32 = 60;
const SECS_PER_HOUR: i32 = 3_600;
const SECS_PER_DAY: i64 = 86_400;
/// `UNIX_EPOCH_JDATE`, the Julian day of 1970-01-01.
const UNIX_EPOCH_JDATE: i32 = 2_440_588;
/// 2000-01-01 00:00:00 UTC in seconds from 1970, where `pg_tz_acceptable` looks for leap seconds.
const TIME_2000: i64 = 946_684_800;
/// `TZ_STRLEN_MAX`, the longest name `pg_tzset` takes.
const TZ_STRLEN_MAX: usize = 255;
/// The largest hour `getsecs` reads, `HOURSPERDAY * DAYSPERWEEK - 1`.
const MAX_RULE_HOUR: i32 = 167;
/// `TZDEFRULESTRING`, the rules of a POSIX zone that names daylight saving time but no rules.
const DEFAULT_RULES: &str = "M3.2.0,M11.1.0";
/// Years after which the Gregorian calendar, and so every POSIX rule, repeats.
const YEARS_PER_REPEAT: i32 = 400;

/// Why PostgreSQL refuses a `TimeZone` setting, one variant per message or detail it raises.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, thiserror::Error)]
pub enum TimeZoneError {
    /// Neither an offset, an IANA name nor a POSIX specification.
    #[error("invalid value for parameter \"TimeZone\"")]
    InvalidValue,
    /// An offset in hours or an interval of 168 hours or more.
    #[error("UTC timezone offset is out of range")]
    OffsetOutOfRange,
    /// An `interval '...'` setting with months.
    #[error("cannot specify months in time zone interval")]
    MonthsInInterval,
    /// An `interval '...'` setting with days.
    #[error("cannot specify days in time zone interval")]
    DaysInInterval,
    /// A zone whose offset at 2000-01-01 00:00:00 UTC has seconds, which PostgreSQL takes for
    /// leap seconds.
    #[error("time zone appears to use leap seconds")]
    LeapSeconds,
    /// An `interval '...'` setting `interval_in` refuses.
    #[error(transparent)]
    Interval(#[from] IntervalError),
}

/// A time zone as PostgreSQL's `TimeZone` setting reads it, equal to another when `SHOW timezone`
/// names both alike.
///
/// ```
/// use sql_values::{PgTimeZone, TimeZoneError};
///
/// assert_eq!(PgTimeZone::parse("europe/rome").unwrap().name(), "Europe/Rome");
/// // A number is hours east of UTC, a POSIX specification hours west of it.
/// assert_eq!(PgTimeZone::parse("+02").unwrap().name(), "<+02>-02");
/// assert_eq!(PgTimeZone::parse("utc+2").unwrap().name(), "UTC+2");
/// assert_eq!(PgTimeZone::parse("+168"), Err(TimeZoneError::OffsetOutOfRange));
/// assert_eq!(PgTimeZone::parse("CEST"), Err(TimeZoneError::InvalidValue));
/// ```
#[derive(Debug, Clone)]
pub struct PgTimeZone {
    name: String,
    rules: Rules,
}

/// How a zone's offset is found.
#[derive(Debug, Clone)]
enum Rules {
    /// One offset east of UTC, in seconds, forever.
    Fixed(i32),
    /// A POSIX specification with daylight saving time.
    Posix(Posix),
    /// An IANA zone with transitions, from jiff's database.
    Iana(TimeZone),
}

/// A POSIX specification with daylight saving time, offsets west of UTC as `tzparse` keeps them.
#[derive(Debug, Clone, Copy)]
struct Posix {
    std_offset: i32,
    dst_offset: i32,
    start: Rule,
    end: Rule,
    /// Whether any year has transitions. A zone without is in daylight saving time throughout.
    switches: bool,
}

/// A POSIX transition rule and its local time of day in seconds.
#[derive(Debug, Clone, Copy)]
struct Rule {
    date: RuleDate,
    time: i32,
}

/// The day a POSIX rule names.
#[derive(Debug, Clone, Copy)]
enum RuleDate {
    /// `Jn`, day 1 to 365 with February 29 never counted.
    Julian(i32),
    /// `n`, day 0 to 365 with February 29 counted.
    DayOfYear(i32),
    /// `Mm.w.d`, day `d` of week `w` of month `m`.
    MonthWeekDay { month: i32, week: i32, weekday: i32 },
}

/// A cursor over a POSIX specification.
struct Spec<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Spec<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    fn eat(&mut self, byte: u8) -> bool {
        let found = self.peek() == Some(byte);
        self.at += usize::from(found);
        found
    }

    fn is_done(&self) -> bool {
        self.at == self.bytes.len()
    }

    /// `getzname` or `getqzname`, the length of the abbreviation read.
    fn name(&mut self) -> Option<usize> {
        if self.eat(b'<') {
            let start = self.at;
            while self.peek().is_some_and(|b| b != b'>') {
                self.at += 1;
            }
            let len = self.at - start;
            self.eat(b'>').then_some(len)
        } else {
            let start = self.at;
            while self
                .peek()
                .is_some_and(|b| !b.is_ascii_digit() && !matches!(b, b',' | b'-' | b'+'))
            {
                self.at += 1;
            }
            Some(self.at - start)
        }
    }

    /// `getnum`, a decimal number within `min..=max`.
    fn number(&mut self, min: i32, max: i32) -> Option<i32> {
        if !self.peek().is_some_and(|b| b.is_ascii_digit()) {
            return None;
        }
        let mut value = 0;
        while let Some(digit) = self.peek().filter(u8::is_ascii_digit) {
            value = value * 10 + i32::from(digit - b'0');
            if value > max {
                return None;
            }
            self.at += 1;
        }
        (value >= min).then_some(value)
    }

    /// `getsecs`, `hh[:mm[:ss]]` in seconds.
    fn seconds(&mut self) -> Option<i32> {
        let mut seconds = self.number(0, MAX_RULE_HOUR)? * SECS_PER_HOUR;
        if self.eat(b':') {
            seconds += self.number(0, 59)? * SECS_PER_MINUTE;
            if self.eat(b':') {
                seconds += self.number(0, 60)?;
            }
        }
        Some(seconds)
    }

    /// `getoffset`, `[+-]hh[:mm[:ss]]` in seconds.
    fn offset(&mut self) -> Option<i32> {
        let negative = self.eat(b'-');
        if !negative {
            self.eat(b'+');
        }
        let seconds = self.seconds()?;
        Some(if negative { -seconds } else { seconds })
    }

    /// `getrule`, `Jn`, `n` or `Mm.w.d` with an optional `/time`, 02:00 by default.
    fn rule(&mut self) -> Option<Rule> {
        let date = if self.eat(b'J') {
            RuleDate::Julian(self.number(1, 365)?)
        } else if self.eat(b'M') {
            let month = self.number(1, 12)?;
            if !self.eat(b'.') {
                return None;
            }
            let week = self.number(1, 5)?;
            if !self.eat(b'.') {
                return None;
            }
            let weekday = self.number(0, 6)?;
            RuleDate::MonthWeekDay {
                month,
                week,
                weekday,
            }
        } else {
            RuleDate::DayOfYear(self.number(0, 365)?)
        };
        let time = if self.eat(b'/') {
            self.offset()?
        } else {
            2 * SECS_PER_HOUR
        };
        Some(Rule { date, time })
    }
}

/// `tzparse` without `lastditch`, the rules of an uppercased POSIX specification.
fn tzparse(spec: &str) -> Option<Rules> {
    let mut cursor = Spec {
        bytes: spec.as_bytes(),
        at: 0,
    };
    cursor.name()?;
    if cursor.is_done() {
        return None;
    }
    let std_offset = cursor.offset()?;
    if cursor.is_done() {
        return Some(Rules::Fixed(-std_offset));
    }
    if cursor.name()? == 0 {
        return None;
    }
    let dst_offset = if cursor.is_done() || matches!(cursor.peek(), Some(b',' | b';')) {
        std_offset - SECS_PER_HOUR
    } else {
        cursor.offset()?
    };
    let mut rules = Spec {
        bytes: DEFAULT_RULES.as_bytes(),
        at: 0,
    };
    let rules = if cursor.is_done() {
        &mut rules
    } else if cursor.eat(b',') || cursor.eat(b';') {
        &mut cursor
    } else {
        return None;
    };
    let start = rules.rule()?;
    if !rules.eat(b',') {
        return None;
    }
    let end = rules.rule()?;
    if !rules.is_done() {
        return None;
    }
    let mut posix = Posix {
        std_offset,
        dst_offset,
        start,
        end,
        switches: false,
    };
    posix.switches = (2000..2000 + YEARS_PER_REPEAT).any(|year| posix.year(year).is_some());
    Some(Rules::Posix(posix))
}

/// C's `isleap`.
const fn is_leap(year: i32) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

impl Rule {
    /// `transtime`, the rule's moment in seconds from January 1 of `year` at 00:00 UTC, with
    /// `offset` seconds west of UTC in effect.
    fn transtime(self, year: i32, offset: i32) -> i64 {
        let leap = is_leap(year);
        let day = match self.date {
            RuleDate::Julian(day) => day - 1 + i32::from(leap && day >= 60),
            RuleDate::DayOfYear(day) => day,
            RuleDate::MonthWeekDay {
                month,
                week,
                weekday,
            } => {
                // Zeller's congruence for the weekday of the month's first day.
                let m1 = (month + 9) % 12 + 1;
                let yy0 = if month <= 2 { year - 1 } else { year };
                let (yy1, yy2) = (yy0 / 100, yy0 % 100);
                let mut dow = ((26 * m1 - 2) / 10 + 1 + yy2 + yy2 / 4 + yy1 / 4 - 2 * yy1) % 7;
                if dow < 0 {
                    dow += 7;
                }
                let mut d = weekday - dow;
                if d < 0 {
                    d += 7;
                }
                let length = days_in_month(year, month);
                for _ in 1..week {
                    if d + 7 >= length {
                        break;
                    }
                    d += 7;
                }
                d + (1..month).map(|m| days_in_month(year, m)).sum::<i32>()
            }
        };
        i64::from(day) * SECS_PER_DAY + i64::from(self.time) + i64::from(offset)
    }
}

impl Posix {
    /// The year's two transitions, in seconds from 1970 and whether daylight saving time follows
    /// each, as `tzparse` lays them out, or `None` when the year has none.
    fn year(self, year: i32) -> Option<[(i64, bool); 2]> {
        // The calendar of the same year in 2000 to 2399, where `tzparse` computes it, since
        // Zeller's congruence in C's arithmetic holds for positive years only.
        let calendar = 2000 + (year - 2000).rem_euclid(YEARS_PER_REPEAT);
        let mut start = self.start.transtime(calendar, self.std_offset);
        let mut end = self.end.transtime(calendar, self.dst_offset);
        let year_secs = if is_leap(calendar) { 366 } else { 365 } * SECS_PER_DAY;
        let reversed = end < start;
        if reversed {
            core::mem::swap(&mut start, &mut end);
        }
        let spans =
            start < end && end - start < year_secs + i64::from(self.std_offset - self.dst_offset);
        if !(reversed || spans) {
            return None;
        }
        let jan_first = i64::from(date2j(year, 1, 1) - UNIX_EPOCH_JDATE) * SECS_PER_DAY;
        Some([(jan_first + start, !reversed), (jan_first + end, reversed)])
    }

    /// The offset east of UTC on either side of a transition.
    const fn offset(self, dst: bool) -> i32 {
        if dst {
            -self.dst_offset
        } else {
            -self.std_offset
        }
    }

    /// The transitions of the years around `year`, in order.
    fn around(self, year: i32) -> impl DoubleEndedIterator<Item = (i64, bool)> {
        (year - 1..=year + 1).flat_map(move |year| self.year(year).into_iter().flatten())
    }

    /// The offset east of UTC in effect at `unix`, as `localsub` finds it.
    fn offset_at(self, unix: i64) -> i32 {
        if !self.switches {
            return self.offset(true);
        }
        let year = year_of(unix);
        (0..=YEARS_PER_REPEAT)
            .find_map(|back| {
                self.around(year - back)
                    .rev()
                    .find(|&(at, _)| at <= unix)
                    .map(|(_, dst)| self.offset(dst))
            })
            .expect("a zone that switches has a transition within 400 years")
    }

    /// The first transition after `unix`, its moment and the offset east after it.
    fn next_transition(self, unix: i64) -> Option<(i64, i32)> {
        if !self.switches {
            return None;
        }
        let year = year_of(unix);
        (0..=YEARS_PER_REPEAT).find_map(|ahead| {
            self.around(year + ahead)
                .find(|&(at, _)| at > unix)
                .map(|(at, dst)| (at, self.offset(dst)))
        })
    }
}

/// The UTC year of `unix` seconds from 1970.
fn year_of(unix: i64) -> i32 {
    let julian = unix.div_euclid(SECS_PER_DAY) + i64::from(UNIX_EPOCH_JDATE);
    j2date(i32::try_from(julian).expect("PostgreSQL's instants lie within int Julian days")).0
}

/// C's `strtod` over the whole of `text`, skipping leading blanks and a `+`, or `None` when
/// characters are left over.
fn strtod_whole(text: &str) -> Option<f64> {
    let bytes = text.as_bytes();
    let mut at = bytes
        .iter()
        .take_while(|&&b| crate::cast::is_pg_space(b))
        .count();
    if bytes.get(at) == Some(&b'+') {
        at += 1;
        if bytes.get(at) == Some(&b'-') {
            return None;
        }
    }
    let (value, end, _) = strtod(bytes, at)?;
    (end == bytes.len()).then_some(value)
}

impl PgTimeZone {
    /// PostgreSQL 18's `check_timezone`, reading an `interval '...'`, else a number of hours east of UTC
    /// as C's `strtod` reads it, else a name `pg_tzset` loads, `GMT`, an IANA name in jiff's
    /// bundled database or a POSIX specification, refused when its offset at 2000-01-01 has
    /// seconds.
    ///
    /// # Errors
    ///
    /// The [`TimeZoneError`] PostgreSQL raises.
    ///
    /// ```
    /// use sql_values::PgTimeZone;
    ///
    /// let name = |setting| PgTimeZone::parse(setting).map(|zone| zone.to_string());
    /// assert_eq!(name("2.5").unwrap(), "<+02:30>-02:30");
    /// assert_eq!(name("interval '-01:00'").unwrap(), "<-01>+01");
    /// assert_eq!(name("est5edt").unwrap(), "EST5EDT");
    /// assert_eq!(name("xxx3yyy").unwrap(), "XXX3YYY");
    /// ```
    pub fn parse(setting: &str) -> Result<Self, TimeZoneError> {
        if setting
            .get(..8)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("interval"))
        {
            let blanks = setting.as_bytes()[8..]
                .iter()
                .take_while(|&&b| crate::cast::is_pg_space(b))
                .count();
            let body = setting[8 + blanks..]
                .strip_prefix('\'')
                .and_then(|rest| rest.strip_suffix('\''))
                .filter(|body| !body.contains('\''))
                .ok_or(TimeZoneError::InvalidValue)?;
            let interval = parse_interval(body)?;
            if interval.months != 0 {
                return Err(TimeZoneError::MonthsInInterval);
            }
            if interval.days != 0 {
                return Err(TimeZoneError::DaysInInterval);
            }
            return Self::from_offset_west(-(interval.microseconds / 1_000_000));
        }
        if let Some(hours) = strtod_whole(setting) {
            // C's conversion of `-hours * SECS_PER_HOUR` to `long` truncates, and one beyond
            // `tzparse`'s 167:59:59 is refused, as is an infinity or NaN.
            let west = -hours * f64::from(SECS_PER_HOUR);
            let limit = f64::from(MAX_RULE_HOUR + 1) * f64::from(SECS_PER_HOUR);
            if !west.is_finite() || west.abs() >= limit {
                return Err(TimeZoneError::OffsetOutOfRange);
            }
            return Self::from_offset_west(crate::float::truncate_to_i64(west));
        }
        let zone = Self::load(setting).ok_or(TimeZoneError::InvalidValue)?;
        if zone
            .utc_offset(TIME_2000)
            .is_some_and(|offset| offset % 60 != 0)
        {
            return Err(TimeZoneError::LeapSeconds);
        }
        Ok(zone)
    }

    /// `pg_tzset_offset`, the zone of a fixed offset `west` seconds west of UTC, named as a
    /// POSIX specification with the offset east in its abbreviation.
    fn from_offset_west(west: i64) -> Result<Self, TimeZoneError> {
        let magnitude = west.unsigned_abs();
        if magnitude >= u64::from((MAX_RULE_HOUR + 1).unsigned_abs()) * 3_600 {
            return Err(TimeZoneError::OffsetOutOfRange);
        }
        let mut clock = alloc::format!("{:02}", magnitude / 3_600);
        if !magnitude.is_multiple_of(3_600) {
            clock += &alloc::format!(":{:02}", magnitude % 3_600 / 60);
            if !magnitude.is_multiple_of(60) {
                clock += &alloc::format!(":{:02}", magnitude % 60);
            }
        }
        let name = if west > 0 {
            alloc::format!("<-{clock}>+{clock}")
        } else {
            alloc::format!("<+{clock}>-{clock}")
        };
        let west = i32::try_from(west).expect("an offset under 168 hours fits int");
        Ok(Self {
            name,
            rules: Rules::Fixed(-west),
        })
    }

    /// `pg_tzset`, loading `GMT`, an IANA name, or an uppercased POSIX specification.
    fn load(name: &str) -> Option<Self> {
        if name.len() > TZ_STRLEN_MAX {
            return None;
        }
        let upper = name.to_ascii_uppercase();
        if upper == "GMT" {
            return Some(Self {
                name: upper,
                rules: Rules::Fixed(0),
            });
        }
        // `tzload` drops one leading colon, and a name that keeps one is no POSIX specification.
        let file = name.strip_prefix(':').unwrap_or(name);
        if let Some(zone) = jiff::tz::db()
            .get(file)
            .ok()
            .filter(|zone| !zone.is_unknown())
        {
            let canonical = zone.iana_name().unwrap_or(file).to_string();
            // A zone with no transitions keeps its one offset beyond jiff's years too.
            let rules = match zone.following(Timestamp::MIN).next() {
                None => Rules::Fixed(zone.to_offset(Timestamp::MIN).seconds()),
                Some(_) => Rules::Iana(zone),
            };
            return Some(Self {
                name: canonical,
                rules,
            });
        }
        if upper.starts_with(':') {
            return None;
        }
        let rules = tzparse(&upper)?;
        Some(Self { name: upper, rules })
    }

    /// The name `SHOW timezone` prints for the zone.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Seconds east of UTC in effect at `unix` seconds from 1970, as `pg_localtime` finds it, or
    /// `None` where an IANA zone is asked about an instant outside jiff's years.
    pub(crate) fn utc_offset(&self, unix: i64) -> Option<i32> {
        match &self.rules {
            Rules::Fixed(offset) => Some(*offset),
            Rules::Posix(posix) => Some(posix.offset_at(unix)),
            Rules::Iana(zone) => Some(zone.to_offset(Timestamp::from_second(unix).ok()?).seconds()),
        }
    }

    /// The offset east of UTC in effect at `unix` and the first transition after it, its moment
    /// and the offset east after it, as `pg_next_dst_boundary` finds them.
    fn next_boundary(&self, unix: i64) -> Option<(i32, Option<(i64, i32)>)> {
        match &self.rules {
            Rules::Fixed(offset) => Some((*offset, None)),
            Rules::Posix(posix) => Some((posix.offset_at(unix), posix.next_transition(unix))),
            Rules::Iana(zone) => {
                let at = Timestamp::from_second(unix).ok()?;
                let next = zone
                    .following(at)
                    .next()
                    .map(|change| (change.timestamp().as_second(), change.offset().seconds()));
                Some((zone.to_offset(at).seconds(), next))
            }
        }
    }

    /// The offset east of UTC that `DetermineTimeZoneOffsetInternal` gives a local clock of
    /// `local` seconds from 1970 read as UTC: the offset at the first transition after the
    /// clock less a day, a clock the change skips taking the offset before it and one it repeats
    /// the offset after it. `None` where an IANA zone is asked about a clock outside jiff's
    /// years.
    pub(crate) fn local_offset(&self, local: i64) -> Option<i32> {
        let (before, next) = self.next_boundary(local - SECS_PER_DAY)?;
        let Some((boundary, after)) = next else {
            return Some(before);
        };
        let before_time = local - i64::from(before);
        let after_time = local - i64::from(after);
        Some(if before_time < boundary && after_time < boundary {
            before
        } else if before_time >= boundary && after_time >= boundary {
            after
        } else if before_time > after_time {
            before
        } else {
            after
        })
    }
}

impl PartialEq for PgTimeZone {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}

impl Eq for PgTimeZone {}

impl Hash for PgTimeZone {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.name.hash(state);
    }
}

impl fmt::Display for PgTimeZone {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name)
    }
}
