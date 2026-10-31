// Times are epoch milliseconds; days and wall-clock times are local.
use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, NaiveDateTime, NaiveTime, SecondsFormat, TimeZone};

pub const DAY: i64 = 86_400_000;

pub fn now() -> i64 {
    Local::now().timestamp_millis()
}

fn local(ms: i64) -> DateTime<Local> {
    DateTime::from_timestamp_millis(ms).unwrap_or_default().with_timezone(&Local)
}

/// A local wall-clock time as epoch ms; a time inside a DST gap moves forward an hour.
pub fn at_local(t: NaiveDateTime) -> i64 {
    Local.from_local_datetime(&t).earliest()
        .or_else(|| Local.from_local_datetime(&(t + Duration::hours(1))).earliest())
        .map_or(0, |d| d.timestamp_millis())
}

pub fn start_of_day(ms: i64) -> i64 {
    at_local(local(ms).date_naive().and_time(NaiveTime::MIN))
}

/// The next local midnight after `ms`, safe across DST.
pub fn next_day(ms: i64) -> i64 {
    at_local((local(ms).date_naive() + Duration::days(1)).and_time(NaiveTime::MIN))
}

pub fn fmt_when(ms: i64) -> String {
    local(ms).format("%a %-d %b, %H:%M").to_string()
}

pub fn fmt_day(ms: i64) -> String {
    local(ms).format("%a %-d %b").to_string()
}

pub fn fmt_hm(ms: i64) -> String {
    local(ms).format("%H:%M").to_string()
}

pub fn hour(ms: i64) -> u32 {
    chrono::Timelike::hour(&local(ms))
}

/// The local midnights of the last `n` days, oldest first, today last.
pub fn last_days(n: usize, now: i64) -> Vec<i64> {
    let mut days = vec![start_of_day(now)];
    while days.len() < n {
        days.push(start_of_day(days[days.len() - 1] - 1));
    }
    days.reverse();
    days
}

pub fn iso(ms: i64) -> String {
    DateTime::from_timestamp_millis(ms).unwrap_or_default().to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// ISO ("2026-10-31", "2026-10-31T09:30"), "+30m" / "in 2h" / "+3d", "today", "tomorrow", weekday names, each with an optional time ("9:30", "5pm").
pub fn parse_when(input: &str, now: i64) -> Result<i64, String> {
    let s = input.trim().to_lowercase();
    if let Some(ms) = relative(&s) {
        return Ok(now + ms);
    }
    let today = local(now).date_naive();
    let mut words: Vec<&str> = s.split_whitespace().filter(|w| *w != "at").collect();
    let mut day = None;
    if let Some(&first) = words.first() {
        // "2026-10-31t09:30" carries its time after the t
        let (d, time) = match first.split_once('t') {
            Some((d, t)) if is_date(d) => (d, Some(t)),
            _ => (first, None),
        };
        day = match d {
            "today" => Some(today),
            "tomorrow" | "tmrw" => Some(today + Duration::days(1)),
            _ if is_date(d) => NaiveDate::parse_from_str(d, "%Y-%m-%d").ok(),
            _ => weekday(d).map(|want| today + Duration::days((want + 7 - today.weekday().num_days_from_sunday() as i64) % 7)),
        };
        if day.is_some() {
            words.remove(0);
            words.extend(time.filter(|t| !t.is_empty()));
        }
    }
    let time = words.concat();
    if day.is_some() || !time.is_empty() {
        // a date with no time means "by end of the workday"
        if let Some((h, m)) = if time.is_empty() { Some((18, 0)) } else { clock(&time) } {
            let date = day.unwrap_or(today);
            let t = at_local(date.and_hms_opt(h, m, 0).unwrap_or_default());
            return Ok(if day.is_none() && t < now { at_local((date + Duration::days(1)).and_hms_opt(h, m, 0).unwrap_or_default()) } else { t });
        }
    }
    DateTime::parse_from_rfc3339(input.trim()).map(|d| d.timestamp_millis()).ok()
        .or_else(|| NaiveDateTime::parse_from_str(input.trim(), "%Y-%m-%dT%H:%M:%S").ok().map(at_local))
        .ok_or_else(|| format!("can't read the date/time \"{input}\" (try 2026-10-31, tomorrow 9:30, fri, +2h)"))
}

fn is_date(s: &str) -> bool {
    s.len() == 10 && s.bytes().enumerate().all(|(i, b)| if i == 4 || i == 7 { b == b'-' } else { b.is_ascii_digit() })
}

fn weekday(s: &str) -> Option<i64> {
    if !s.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }
    ["sun", "mon", "tue", "wed", "thu", "fri", "sat"].iter().position(|p| s.starts_with(p)).map(|i| i as i64)
}

/// "+30m", "in 2h", "+3d" → milliseconds.
fn relative(s: &str) -> Option<i64> {
    let rest = s.strip_prefix('+').or_else(|| s.strip_prefix("in "))?.trim_start();
    let (n, unit) = rest.split_at(rest.find(|c: char| !c.is_ascii_digit())?);
    let unit = match unit.trim_start() {
        "m" | "min" | "mins" | "minute" | "minutes" => 60_000,
        "h" | "hr" | "hrs" | "hour" | "hours" => 3_600_000,
        "d" | "day" | "days" => DAY,
        _ => return None,
    };
    Some(n.parse::<i64>().ok()? * unit)
}

/// "9", "9:30", "5pm", "12am" → (hour, minute).
fn clock(s: &str) -> Option<(u32, u32)> {
    let (s, pm) = match (s.strip_suffix("am"), s.strip_suffix("pm")) {
        (Some(x), _) => (x, Some(false)),
        (_, Some(x)) => (x, Some(true)),
        _ => (s, None),
    };
    let (h, m) = s.split_once(':').unwrap_or((s, "00"));
    if h.is_empty() || h.len() > 2 || m.len() != 2 || !h.bytes().chain(m.bytes()).all(|b| b.is_ascii_digit()) {
        return None;
    }
    let (mut h, m): (u32, u32) = (h.parse().ok()?, m.parse().ok()?);
    match pm {
        Some(true) if h < 12 => h += 12,
        Some(false) if h == 12 => h = 0,
        _ => {}
    }
    (h <= 23 && m <= 59).then_some((h, m))
}

#[cfg(test)]
pub fn at(s: &str) -> i64 {
    at_local(NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M").unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_when_reads_the_short_forms() {
        let now = at("2026-10-28T10:00"); // a Wednesday
        assert_eq!(parse_when("fri", now), Ok(at("2026-10-30T18:00")));
        assert_eq!(parse_when("tomorrow 9:30", now), Ok(at("2026-10-29T09:30")));
        assert_eq!(parse_when("+2h", now), Ok(at("2026-10-28T12:00")));
        assert_eq!(parse_when("9am", now), Ok(at("2026-10-29T09:00"))); // already past today, so tomorrow
        assert_eq!(parse_when("fri at 5 pm", now), Ok(at("2026-10-30T17:00")));
        assert_eq!(parse_when("2026-10-31T09:30", now), Ok(at("2026-10-31T09:30")));
        assert_eq!(parse_when("2026-11-02", now), Ok(at("2026-11-02T18:00")));
        assert!(parse_when("someday", now).is_err());
        assert!(parse_when("25:00", now).is_err());
    }
}
