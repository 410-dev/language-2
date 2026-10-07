//! Clocks, calendar arithmetic, date formatting and IANA time zones (proleptic Gregorian
//! calendar). Zone rules come from `jiff`: the system's zoneinfo database on Unix, a bundled
//! copy on Windows.

use super::{int_array, opt_str, str_array, tuple, Args, SysOp};
use crate::value::*;
use jiff::tz::TimeZone;

const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
const DAYS: [&str; 7] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];

pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

pub fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        2 if is_leap(y) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// [year, month, day, hour, minute, second, millisecond, dayOfWeek (1 = Monday), dayOfYear]
pub fn fields(millis: i64, offset: i64) -> [i64; 9] {
    let local = millis + offset * 1000;
    let days = local.div_euclid(86_400_000);
    let rem = local.rem_euclid(86_400_000);
    let (y, m, d) = civil_from_days(days);
    let dow = (days + 3).rem_euclid(7) + 1;
    let doy = days - days_from_civil(y, 1, 1) + 1;
    [y, m, d, rem / 3_600_000, rem / 60_000 % 60, rem / 1000 % 60, rem % 1000, dow, doy]
}

fn check_fields(f: &[i64]) -> Result<(), String> {
    let g = |i: usize| f.get(i).copied().unwrap_or(0);
    let (y, m, d) = (g(0), g(1), g(2));
    if !(1..=12).contains(&m) {
        return Err(format!("month {} out of range 1..12", m));
    }
    if d < 1 || d > days_in_month(y, m) {
        return Err(format!("day {} out of range for {}-{:02}", d, y, m));
    }
    for (i, (name, hi)) in [("hour", 23), ("minute", 59), ("second", 59), ("millisecond", 999)].iter().enumerate() {
        let v = g(3 + i);
        if v < 0 || v > *hi {
            return Err(format!("{} {} out of range 0..{}", name, v, hi));
        }
    }
    Ok(())
}

pub fn from_fields(f: &[i64], offset: i64) -> i64 {
    let g = |i: usize| f.get(i).copied().unwrap_or(0);
    let days = days_from_civil(g(0), g(1), g(2));
    days * 86_400_000 + g(3) * 3_600_000 + g(4) * 60_000 + g(5) * 1000 + g(6) - offset * 1000
}

/// The zone named `name` ("Asia/Seoul", "UTC", ...); `None` = the system's local zone.
pub fn zone(name: Option<&str>) -> Result<TimeZone, String> {
    match name {
        None => Ok(TimeZone::system()),
        Some(n) => TimeZone::get(n).map_err(|_| format!("unknown time zone \"{}\" (use an IANA name such as \"Asia/Seoul\" or \"UTC\")", n)),
    }
}

fn timestamp(millis: i64) -> jiff::Timestamp {
    jiff::Timestamp::from_millisecond(millis).unwrap_or(jiff::Timestamp::UNIX_EPOCH)
}

/// UTC offset (seconds) of `tz` at an instant.
pub fn zone_offset(tz: &TimeZone, millis: i64) -> i64 {
    tz.to_offset(timestamp(millis)).seconds() as i64
}

/// The instant (millis) and offset of a wall-clock time in `tz`. A time that occurs twice
/// (clocks set back) takes the earlier offset; a skipped time (clocks set forward) moves
/// forward by the length of the gap, like Java's ZonedDateTime.
fn zone_resolve(tz: &TimeZone, f: &[i64]) -> (i64, i64) {
    let g = |i: usize| f.get(i).copied().unwrap_or(0);
    let dt = jiff::civil::DateTime::new(g(0) as i16, g(1) as i8, g(2) as i8, g(3) as i8, g(4) as i8, g(5) as i8, (g(6) * 1_000_000) as i32);
    match dt.ok().and_then(|dt| tz.to_ambiguous_zoned(dt).compatible().ok()) {
        Some(z) => (z.timestamp().as_millisecond(), z.offset().seconds() as i64),
        None => {
            let off = zone_offset(tz, from_fields(f, 0));
            (from_fields(f, off), off)
        }
    }
}

fn local_resolve(f: &[i64]) -> (i64, i64) {
    zone_resolve(&TimeZone::system(), f)
}

fn abbreviation(tz: &TimeZone, millis: i64) -> String {
    tz.to_offset_info(timestamp(millis)).abbreviation().to_string()
}

fn offset_text(off: i64, colon: bool, z: bool) -> String {
    if off == 0 && z {
        return "Z".into();
    }
    let sign = if off < 0 { '-' } else { '+' };
    let a = off.abs();
    if colon {
        format!("{}{:02}:{:02}", sign, a / 3600, a / 60 % 60)
    } else {
        format!("{}{:02}{:02}", sign, a / 3600, a / 60 % 60)
    }
}

/// Pattern tokens: `yyyy yy MMMM MMM MM M dd d HH H hh h mm m ss s SSS a EEEE EEE XXX Z`,
/// text in single quotes (`''` is a quote).
fn tokens(pattern: &str) -> Vec<(String, bool)> {
    let cs: Vec<char> = pattern.chars().collect();
    let mut out: Vec<(String, bool)> = Vec::new();
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        if c == '\'' {
            let mut lit = String::new();
            i += 1;
            while i < cs.len() {
                if cs[i] == '\'' {
                    if i + 1 < cs.len() && cs[i + 1] == '\'' {
                        lit.push('\'');
                        i += 2;
                        continue;
                    }
                    break;
                }
                lit.push(cs[i]);
                i += 1;
            }
            if lit.is_empty() {
                lit.push('\'');
            }
            out.push((lit, true));
            i += 1;
        } else if c.is_ascii_alphabetic() {
            let mut j = i;
            while j < cs.len() && cs[j] == c {
                j += 1;
            }
            out.push((cs[i..j].iter().collect(), false));
            i = j;
        } else {
            out.push((c.to_string(), true));
            i += 1;
        }
    }
    out
}

pub fn format(millis: i64, offset: i64, pattern: &str, zone_name: Option<&str>) -> Result<String, String> {
    let f = fields(millis, offset);
    let (y, mo, d, hh, mi, ss, ms, dow) = (f[0], f[1], f[2], f[3], f[4], f[5], f[6], f[7]);
    let h12 = if hh % 12 == 0 { 12 } else { hh % 12 };
    let mut out = String::new();
    for (t, lit) in tokens(pattern) {
        if lit {
            out.push_str(&t);
            continue;
        }
        let s = match t.as_str() {
            "yyyy" => format!("{:04}", y),
            "yy" => format!("{:02}", y.rem_euclid(100)),
            "y" => y.to_string(),
            "MMMM" => MONTHS[(mo - 1) as usize].to_string(),
            "MMM" => MONTHS[(mo - 1) as usize][..3].to_string(),
            "MM" => format!("{:02}", mo),
            "M" => mo.to_string(),
            "dd" => format!("{:02}", d),
            "d" => d.to_string(),
            "HH" => format!("{:02}", hh),
            "H" => hh.to_string(),
            "hh" => format!("{:02}", h12),
            "h" => h12.to_string(),
            "mm" => format!("{:02}", mi),
            "m" => mi.to_string(),
            "ss" => format!("{:02}", ss),
            "s" => ss.to_string(),
            "SSS" => format!("{:03}", ms),
            "SS" => format!("{:02}", ms / 10),
            "S" => (ms / 100).to_string(),
            "a" => (if hh < 12 { "AM" } else { "PM" }).to_string(),
            "EEEE" => DAYS[(dow - 1) as usize].to_string(),
            "EEE" => DAYS[(dow - 1) as usize][..3].to_string(),
            "XXX" => offset_text(offset, true, true),
            "xxx" => offset_text(offset, true, false),
            "Z" => offset_text(offset, false, false),
            "VV" => match zone_name {
                Some(z) => z.to_string(),
                None => offset_text(offset, true, true),
            },
            "z" | "zzz" => match zone_name {
                Some(z) => abbreviation(&zone(Some(z))?, millis),
                None => format!("UTC{}", if offset == 0 { String::new() } else { offset_text(offset, true, false) }),
            },
            other => return Err(format!("unknown date pattern letter(s) '{}' (quote literal text with '...')", other)),
        };
        out.push_str(&s);
    }
    Ok(out)
}

/// Parses `text` with `pattern`: ([epochMillis, offsetSeconds], zone). Without an offset or zone
/// in the text, it is a wall-clock time in `default_zone` (`None` = local).
pub fn parse(text: &str, pattern: &str, default_zone: Option<&str>) -> Result<([i64; 2], Option<String>), String> {
    let t: Vec<char> = text.chars().collect();
    let mut i = 0;
    let mut f = [1970i64, 1, 1, 0, 0, 0, 0];
    let mut pm: Option<bool> = None;
    let mut offset: Option<i64> = None;
    let mut zone_name: Option<String> = None;
    let bad = |what: &str| format!("cannot parse \"{}\" with pattern \"{}\": {}", text, pattern, what);
    let num = |i: &mut usize, min: usize, max: usize| -> Option<i64> {
        let start = *i;
        let neg = *i < t.len() && t[*i] == '-' && max > 2;
        if neg {
            *i += 1;
        }
        let ds = *i;
        while *i < t.len() && *i - ds < max && t[*i].is_ascii_digit() {
            *i += 1;
        }
        if *i - ds < min {
            *i = start;
            return None;
        }
        let v: i64 = t[ds..*i].iter().collect::<String>().parse().ok()?;
        Some(if neg { -v } else { v })
    };
    let word = |i: &mut usize, names: &[&str], short: bool| -> Option<i64> {
        let rest: String = t[*i..].iter().collect::<String>().to_lowercase();
        for (k, n) in names.iter().enumerate() {
            let n = if short { &n[..3] } else { n };
            if rest.starts_with(&n.to_lowercase()) {
                *i += n.chars().count();
                return Some(k as i64 + 1);
            }
        }
        None
    };
    for (tok, lit) in tokens(pattern) {
        if lit {
            for c in tok.chars() {
                if i < t.len() && t[i] == c {
                    i += 1;
                } else {
                    return Err(bad(&format!("expected '{}'", c)));
                }
            }
            continue;
        }
        let v = match tok.as_str() {
            "yyyy" | "y" => num(&mut i, 1, 9).map(|v| f[0] = v),
            "yy" => num(&mut i, 2, 2).map(|v| f[0] = 2000 + v),
            "MMMM" => word(&mut i, &MONTHS, false).map(|v| f[1] = v),
            "MMM" => word(&mut i, &MONTHS, true).map(|v| f[1] = v),
            "MM" | "M" => num(&mut i, 1, 2).map(|v| f[1] = v),
            "dd" | "d" => num(&mut i, 1, 2).map(|v| f[2] = v),
            "HH" | "H" | "hh" | "h" => num(&mut i, 1, 2).map(|v| f[3] = v),
            "mm" | "m" => num(&mut i, 1, 2).map(|v| f[4] = v),
            "ss" | "s" => num(&mut i, 1, 2).map(|v| f[5] = v),
            "SSS" => num(&mut i, 3, 3).map(|v| f[6] = v),
            "SS" => num(&mut i, 2, 2).map(|v| f[6] = v * 10),
            "S" => num(&mut i, 1, 1).map(|v| f[6] = v * 100),
            "a" => {
                let rest: String = t[i..].iter().take(2).collect::<String>().to_uppercase();
                let half = match rest.as_str() {
                    "AM" => Some(false),
                    "PM" => Some(true),
                    _ => None,
                };
                half.map(|p| {
                    pm = Some(p);
                    i += 2;
                })
            }
            "EEEE" => word(&mut i, &DAYS, false).map(|_| ()),
            "EEE" => word(&mut i, &DAYS, true).map(|_| ()),
            "XXX" | "xxx" | "Z" => {
                if i < t.len() && (t[i] == 'Z' || t[i] == 'z') {
                    i += 1;
                    offset = Some(0);
                    Some(())
                } else if i < t.len() && (t[i] == '+' || t[i] == '-') {
                    let sign = if t[i] == '-' { -1 } else { 1 };
                    i += 1;
                    let hh = num(&mut i, 2, 2);
                    if i < t.len() && t[i] == ':' {
                        i += 1;
                    }
                    let mm = num(&mut i, 2, 2);
                    match (hh, mm) {
                        (Some(a), Some(b)) => {
                            offset = Some(sign * (a * 3600 + b * 60));
                            Some(())
                        }
                        _ => None,
                    }
                } else {
                    None
                }
            }
            "VV" => {
                let start = i;
                while i < t.len() && (t[i].is_ascii_alphanumeric() || "_/+-".contains(t[i])) {
                    i += 1;
                }
                let name: String = t[start..i].iter().collect();
                if name.is_empty() {
                    None
                } else {
                    zone(Some(&name))?;
                    zone_name = Some(name);
                    Some(())
                }
            }
            other => return Err(format!("unknown date pattern letter(s) '{}'", other)),
        };
        if v.is_none() {
            return Err(bad(&format!("no value for '{}' at offset {}", tok, i)));
        }
    }
    if i != t.len() {
        return Err(bad("unexpected trailing text"));
    }
    if let Some(p) = pm {
        if !(1..=12).contains(&f[3]) {
            return Err(bad("12-hour clock hour out of range"));
        }
        f[3] = f[3] % 12 + if p { 12 } else { 0 };
    }
    check_fields(&f).map_err(|e| bad(&e))?;
    let zone_name = zone_name.or_else(|| if offset.is_none() { default_zone.map(|z| z.to_string()) } else { None });
    let (millis, off) = match (offset, &zone_name) {
        (Some(o), _) => (from_fields(&f, o), o),
        (None, Some(z)) => zone_resolve(&zone(Some(z))?, &f),
        (None, None) => local_resolve(&f),
    };
    Ok(([millis, off], zone_name))
}

/// Go-style duration text: `1h2m3.5s`, `1.5ms`, `250µs`, `42ns`.
pub fn duration_string(nanos: i64) -> String {
    if nanos == 0 {
        return "0s".into();
    }
    let neg = nanos < 0;
    let n = nanos.unsigned_abs();
    let frac = |v: u64, unit: u64| {
        let whole = v / unit;
        let rem = v % unit;
        if rem == 0 {
            whole.to_string()
        } else {
            let digits = unit.to_string().len() - 1;
            let s = format!("{}.{:0w$}", whole, rem, w = digits);
            s.trim_end_matches('0').to_string()
        }
    };
    let body = if n < 1_000 {
        format!("{}ns", n)
    } else if n < 1_000_000 {
        format!("{}µs", frac(n, 1_000))
    } else if n < 1_000_000_000 {
        format!("{}ms", frac(n, 1_000_000))
    } else {
        let secs = n / 1_000_000_000;
        let sub = n % 1_000_000_000;
        let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
        let mut out = String::new();
        if h > 0 {
            out.push_str(&format!("{}h", h));
        }
        if h > 0 || m > 0 {
            out.push_str(&format!("{}m", m));
        }
        out.push_str(&format!("{}s", frac(s * 1_000_000_000 + sub, 1_000_000_000)));
        out
    };
    if neg {
        format!("-{}", body)
    } else {
        body
    }
}

fn monotonic_base() -> std::time::Instant {
    static BASE: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    *BASE.get_or_init(std::time::Instant::now)
}

pub(super) fn call<H: Host>(op: SysOp, a: &Args, h: &mut H) -> Result<Value, H::Err> {
    use SysOp::*;
    let ill = |h: &mut H, e: String| h.throw(ExcKind::IllegalArgument, e);
    Ok(match op {
        timeNowMillis => Value::i64(match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => d.as_millis() as i64,
            Err(e) => -(e.duration().as_millis() as i64),
        }),
        timeMonotonicNanos => Value::i64(monotonic_base().elapsed().as_nanos() as i64),
        timeFields => int_array(fields(a.int(0), a.int(1)).to_vec()),
        timeFromFields => {
            let f = a.ints(0);
            check_fields(&f).map_err(|e| ill(h, e))?;
            Value::i64(from_fields(&f, a.int(1)))
        }
        timeAddCalendar => {
            // millis, offset, months, days, zone: the same wall-clock time on another date
            let (millis, off, months, days) = (a.int(0), a.int(1), a.int(2), a.int(3));
            let f = fields(millis, off);
            let total = f[0] * 12 + (f[1] - 1) + months;
            let (y, m) = (total.div_euclid(12), total.rem_euclid(12) + 1);
            let d = f[2].min(days_in_month(y, m));
            let (y, m, d) = civil_from_days(days_from_civil(y, m, d) + days);
            let nf = [y, m, d, f[3], f[4], f[5], f[6]];
            let (nm, noff) = match a.opt_str(4) {
                Some(z) => zone_resolve(&zone(Some(&z)).map_err(|e| ill(h, e))?, &nf),
                None => (from_fields(&nf, off), off),
            };
            tuple(vec![Value::i64(nm), Value::i64(noff)])
        }
        timeFormat => Value::str(format(a.int(0), a.int(1), &a.str(2), a.opt_str(3).as_deref()).map_err(|e| ill(h, e))?),
        timeParse => {
            let (r, z) = parse(&a.str(0), &a.str(1), a.opt_str(2).as_deref()).map_err(|e| ill(h, e))?;
            tuple(vec![int_array(r.to_vec()), opt_str(z)])
        }
        tzLocalName => opt_str(TimeZone::system().iana_name().map(|s| s.to_string())),
        tzAvailable => {
            let mut names: Vec<String> = jiff::tz::db().available().map(|n| n.to_string()).collect();
            names.sort();
            str_array(names)
        }
        tzValid => Value::Bool(zone(Some(&a.str(0))).is_ok()),
        tzOffset => {
            let tz = zone(a.opt_str(0).as_deref()).map_err(|e| ill(h, e))?;
            Value::i64(zone_offset(&tz, a.int(1)))
        }
        tzOffsetOf => {
            let f = a.ints(1);
            check_fields(&f).map_err(|e| ill(h, e))?;
            let tz = zone(a.opt_str(0).as_deref()).map_err(|e| ill(h, e))?;
            let (m, o) = zone_resolve(&tz, &f);
            tuple(vec![Value::i64(m), Value::i64(o)])
        }
        tzAbbreviation => {
            let tz = zone(a.opt_str(0).as_deref()).map_err(|e| ill(h, e))?;
            Value::str(abbreviation(&tz, a.int(1)))
        }
        durationString => Value::str(duration_string(a.int(0))),
        _ => unreachable!(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calendar() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil_from_days(19_000), (2022, 1, 8));
        for z in [-1_000_000, -1, 0, 1, 59, 60, 365, 11_016, 1_000_000] {
            let (y, m, d) = civil_from_days(z);
            assert_eq!(days_from_civil(y, m, d), z);
        }
        let f = fields(1_700_000_000_123, 9 * 3600);
        assert_eq!(&f[..7], &[2023, 11, 15, 7, 13, 20, 123]);
        assert_eq!(f[7], 3); // Wednesday
        assert_eq!(format(1_700_000_000_123, 9 * 3600, "yyyy-MM-dd'T'HH:mm:ss.SSSXXX EEE MMM a", None).unwrap(), "2023-11-15T07:13:20.123+09:00 Wed Nov AM");
        assert_eq!(parse("2023-11-15T07:13:20.123+09:00", "yyyy-MM-dd'T'HH:mm:ss.SSSXXX", None).unwrap().0, [1_700_000_000_123, 32400]);
        // New York: EST -5 in winter, EDT -4 in summer; 2026-03-08 02:30 does not exist
        let ny = zone(Some("America/New_York")).unwrap();
        assert_eq!(zone_offset(&ny, 1_767_225_600_000), -5 * 3600);
        assert_eq!(zone_offset(&ny, 1_783_000_000_000), -4 * 3600);
        // 02:30 is skipped: it becomes 03:30 EDT (07:30Z)
        assert_eq!(zone_resolve(&ny, &[2026, 3, 8, 2, 30, 0, 0]), (from_fields(&[2026, 3, 8, 7, 30, 0, 0], 0), -4 * 3600));
        assert_eq!(format(1_783_000_000_000, -4 * 3600, "VV zzz", Some("America/New_York")).unwrap(), "America/New_York EDT");
        assert_eq!(duration_string(3_723_500_000_000), "1h2m3.5s");
        assert_eq!(duration_string(1_500_000), "1.5ms");
        assert_eq!(duration_string(-42), "-42ns");
    }
}
