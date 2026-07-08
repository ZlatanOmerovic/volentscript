//! Date instances (ES3 §15.9, AS3 Date class).
//!
//! A Date is a single time value: milliseconds since the epoch, or NaN
//! for an invalid date (§15.9.1.1). Local-time conversions go through
//! `chrono::Local` (the platform timezone database). Getters funnel
//! through one indexed accessor and the string forms through one indexed
//! formatter, mirroring avmplus `Date::getDateProperty` /
//! `Date::toString` (core/Date.cpp).

use std::cell::Cell;

use chrono::{Datelike, Local, Offset, TimeZone, Timelike, Utc};

use crate::gc;
use crate::string::VsString;

/// The Date runtime object (GC Raw block: one number, nothing to trace).
pub struct VsDate {
    /// Milliseconds since epoch, or NaN = Invalid Date.
    pub millis: Cell<f64>,
}

/// §15.9.1.14 TimeClip: NaN outside ±8.64e15, else truncate toward zero.
fn time_clip(t: f64) -> f64 {
    if !t.is_finite() || t.abs() > 8.64e15 {
        f64::NAN
    } else {
        t.trunc()
    }
}

/// Allocates a Date holding `millis` (already clipped).
pub fn alloc(millis: f64) -> *const VsDate {
    let p = gc::alloc(std::mem::size_of::<VsDate>(), gc::Kind::Raw) as *mut VsDate;
    // SAFETY: fresh block of exactly VsDate size.
    unsafe {
        p.write(VsDate {
            millis: Cell::new(time_clip(millis)),
        })
    };
    p
}

/// `new Date()` — the current time.
pub fn now_ms() -> f64 {
    Utc::now().timestamp_millis() as f64
}

/// §15.9.3.1 with 2..=7 args: local civil components → time value.
/// Year 0..=99 reads as 1900+year (MakeFullYear).
pub fn from_components(parts: &[f64]) -> f64 {
    if parts.iter().any(|v| !v.is_finite()) {
        return f64::NAN;
    }
    let year = {
        let y = parts[0].trunc();
        if (0.0..=99.0).contains(&y) {
            1900.0 + y
        } else {
            y
        }
    };
    let month = parts.get(1).map_or(0.0, |v| v.trunc());
    let day = parts.get(2).map_or(1.0, |v| v.trunc());
    let hour = parts.get(3).map_or(0.0, |v| v.trunc());
    let min = parts.get(4).map_or(0.0, |v| v.trunc());
    let sec = parts.get(5).map_or(0.0, |v| v.trunc());
    let ms = parts.get(6).map_or(0.0, |v| v.trunc());
    // MakeDay handles out-of-range months/days by rolling over; lean on
    // the epoch-arithmetic identity instead of chrono's checked ctors.
    let utc = utc_from_civil(year, month, day, hour, min, sec, ms);
    // The components are local time: subtract the offset that applies at
    // that local time. Approximate the ES3 LocalTime inverse by probing
    // the offset at the UTC interpretation (correct except within the
    // hour around a DST transition — avmplus does the same single probe).
    let probe = Local
        .timestamp_millis_opt(utc as i64)
        .single()
        .map(|dt| dt.offset().fix().local_minus_utc())
        .unwrap_or(0);
    time_clip(utc - f64::from(probe) * 1000.0)
}

/// Same, but the components are UTC (`Date.UTC`, §15.9.4.3).
pub fn utc_from_parts(parts: &[f64]) -> f64 {
    if parts.iter().any(|v| !v.is_finite()) {
        return f64::NAN;
    }
    let year = {
        let y = parts[0].trunc();
        if (0.0..=99.0).contains(&y) {
            1900.0 + y
        } else {
            y
        }
    };
    time_clip(utc_from_civil(
        year,
        parts.get(1).map_or(0.0, |v| v.trunc()),
        parts.get(2).map_or(1.0, |v| v.trunc()),
        parts.get(3).map_or(0.0, |v| v.trunc()),
        parts.get(4).map_or(0.0, |v| v.trunc()),
        parts.get(5).map_or(0.0, |v| v.trunc()),
        parts.get(6).map_or(0.0, |v| v.trunc()),
    ))
}

/// §15.9.1.12/13 MakeDay+MakeDate as pure arithmetic (months roll over).
fn utc_from_civil(year: f64, month: f64, day: f64, h: f64, m: f64, s: f64, ms: f64) -> f64 {
    let ym = year + (month / 12.0).floor();
    let mn = month.rem_euclid(12.0);
    let days = days_from_civil(ym as i64, mn as u32) + (day - 1.0) as i64;
    days as f64 * 86_400_000.0 + h * 3_600_000.0 + m * 60_000.0 + s * 1000.0 + ms
}

/// Days from epoch to year/month (month 0-11, day 1) — Howard Hinnant's
/// civil-days algorithm (public domain), also §15.9.1.3 DayFromYear.
fn days_from_civil(y: i64, m: u32) -> i64 {
    let y = y - i64::from(m < 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let mp = (m + 10) % 12;
    let doy = (153 * mp as u64 + 2) / 5;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe as i64 - 719_468
}

/// Getter indices (one accessor like avmplus `getDateProperty`):
/// 0 time, 1..=8 local fullYear/month/date/day/hours/minutes/seconds/ms,
/// 9..=16 same in UTC, 17 timezoneOffset (minutes).
pub fn get(d: &VsDate, index: u32) -> f64 {
    let t = d.millis.get();
    if t.is_nan() {
        return f64::NAN;
    }
    if index == 0 {
        return t;
    }
    if index == 17 {
        let off = Local
            .timestamp_millis_opt(t as i64)
            .single()
            .map(|dt| dt.offset().fix().local_minus_utc())
            .unwrap_or(0);
        // §15.9.5.26: (t - LocalTime(t)) in minutes.
        return f64::from(-off) / 60.0;
    }
    let utc = Utc.timestamp_millis_opt(t as i64).single();
    let Some(utc) = utc else { return f64::NAN };
    let (field, local) = if index <= 8 {
        (index, true)
    } else {
        (index - 8, false)
    };
    macro_rules! read {
        ($dt:expr) => {{
            let dt = $dt;
            match field {
                1 => f64::from(dt.year()),
                2 => f64::from(dt.month0()),
                3 => f64::from(dt.day()),
                4 => f64::from(dt.weekday().num_days_from_sunday()),
                5 => f64::from(dt.hour()),
                6 => f64::from(dt.minute()),
                7 => f64::from(dt.second()),
                8 => f64::from(dt.timestamp_subsec_millis()),
                _ => f64::NAN,
            }
        }};
    }
    if local {
        read!(utc.with_timezone(&Local))
    } else {
        read!(utc)
    }
}

/// setTime (§15.9.5.27): clips and stores; returns the stored value.
pub fn set_time(d: &VsDate, value: f64) -> f64 {
    let v = time_clip(value);
    d.millis.set(v);
    v
}

const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// String forms; `index` keeps the avmplus numbering (core/Date.cpp
/// Date::toString): 0 toString, 1 toDateString, 2 toTimeString,
/// 6 toUTCString.
pub fn to_string(d: &VsDate, index: u32) -> *const VsString {
    let t = d.millis.get();
    if t.is_nan() {
        return VsString::from_rust("Invalid Date");
    }
    let utc = match Utc.timestamp_millis_opt(t as i64).single() {
        Some(v) => v,
        None => return VsString::from_rust("Invalid Date"),
    };
    let s = if index == 6 {
        // "%3 %3 %d %2:%2:%2 %d UTC" (AS3-mode kToUTCString).
        format!(
            "{} {} {} {:02}:{:02}:{:02} {} UTC",
            DAYS[utc.weekday().num_days_from_sunday() as usize],
            MONTHS[utc.month0() as usize],
            utc.day(),
            utc.hour(),
            utc.minute(),
            utc.second(),
            utc.year()
        )
    } else {
        let loc = utc.with_timezone(&Local);
        let off = loc.offset().fix().local_minus_utc();
        let (sign, off) = if off < 0 { ('-', -off) } else { ('+', off) };
        let (oh, om) = (off / 3600, off % 3600 / 60);
        match index {
            // "%3 %3 %d %2:%2:%2 GMT%c%2%2 %d" (kToString).
            0 => format!(
                "{} {} {} {:02}:{:02}:{:02} GMT{}{:02}{:02} {}",
                DAYS[loc.weekday().num_days_from_sunday() as usize],
                MONTHS[loc.month0() as usize],
                loc.day(),
                loc.hour(),
                loc.minute(),
                loc.second(),
                sign,
                oh,
                om,
                loc.year()
            ),
            // "%3 %3 %d %d" (kToDateString).
            1 => format!(
                "{} {} {} {}",
                DAYS[loc.weekday().num_days_from_sunday() as usize],
                MONTHS[loc.month0() as usize],
                loc.day(),
                loc.year()
            ),
            // "%2:%2:%2 GMT%c%2%2" (kToTimeString).
            _ => format!(
                "{:02}:{:02}:{:02} GMT{}{:02}{:02}",
                loc.hour(),
                loc.minute(),
                loc.second(),
                sign,
                oh,
                om
            ),
        }
    };
    VsString::from_rust(&s)
}
