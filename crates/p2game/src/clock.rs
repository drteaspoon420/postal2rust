//! The LevelInfo wall clock (`Year`, `Month`, `Day`, `DayOfWeek`, `Hour`, `Minute`, `Second`,
//! `Millisecond`): the engine fills these with the local time every tick. Script reads them for
//! save slot timestamps (`P2GameInfoSingle.SaveGame`) and timers (`GetMillisecondsNow`).

use uscript::vm::{ObjId, Value, Vm};

/// Local calendar time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Clock {
    pub year: i32,
    pub month: i32,
    pub day: i32,
    pub day_of_week: i32,
    pub hour: i32,
    pub minute: i32,
    pub second: i32,
    pub millisecond: i32,
}

#[cfg(windows)]
pub fn now() -> Clock {
    #[repr(C)]
    #[derive(Default)]
    struct SystemTime {
        year: u16,
        month: u16,
        day_of_week: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
        milliseconds: u16,
    }
    unsafe extern "system" {
        fn GetLocalTime(t: *mut SystemTime);
    }
    let mut t = SystemTime::default();
    // SAFETY: GetLocalTime only writes the SYSTEMTIME it is given.
    unsafe { GetLocalTime(&mut t) };
    Clock {
        year: t.year as i32,
        month: t.month as i32,
        day: t.day as i32,
        day_of_week: t.day_of_week as i32,
        hour: t.hour as i32,
        minute: t.minute as i32,
        second: t.second as i32,
        millisecond: t.milliseconds as i32,
    }
}

#[cfg(unix)]
pub fn now() -> Clock {
    // libc `localtime_r` (std links libc already). `struct tm` starts with nine ints (sec, min,
    // hour, mday, mon, year, wday, yday, isdst) on glibc, musl and macOS; the buffer is larger
    // than any `struct tm`.
    unsafe extern "C" {
        fn localtime_r(t: *const i64, out: *mut [i32; 16]) -> *mut [i32; 16];
    }
    let d = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let secs = d.as_secs() as i64;
    let mut tm = [0i32; 16];
    // SAFETY: time_t is 64-bit on every supported unix target; tm is a large enough buffer.
    if unsafe { localtime_r(&secs, &mut tm) }.is_null() {
        return from_unix(secs, d.subsec_millis() as i32);
    }
    Clock {
        year: tm[5] + 1900,
        month: tm[4] + 1,
        day: tm[3],
        day_of_week: tm[6],
        hour: tm[2],
        minute: tm[1],
        second: tm[0].min(59),
        millisecond: d.subsec_millis() as i32,
    }
}

#[cfg(not(any(windows, unix)))]
pub fn now() -> Clock {
    // UTC: no time zone database here.
    let d = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    from_unix(d.as_secs() as i64, d.subsec_millis() as i32)
}

/// Civil date from Unix seconds (UTC).
pub fn from_unix(secs: i64, ms: i32) -> Clock {
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    // Howard Hinnant's days-to-civil algorithm.
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + (month <= 2) as i64;
    Clock {
        year: year as i32,
        month: month as i32,
        day: day as i32,
        day_of_week: (days + 4).rem_euclid(7) as i32,
        hour: (rem / 3600) as i32,
        minute: (rem / 60 % 60) as i32,
        second: (rem % 60) as i32,
        millisecond: ms,
    }
}

/// Writes the clock into a LevelInfo.
pub fn apply(vm: &mut Vm, level: ObjId, c: Clock) {
    for (n, v) in [("Year", c.year), ("Month", c.month), ("Day", c.day), ("DayOfWeek", c.day_of_week), ("Hour", c.hour), ("Minute", c.minute), ("Second", c.second), ("Millisecond", c.millisecond)] {
        vm.set_prop(level, n, Value::Int(v));
    }
}

fn level_hook(host: &crate::host::GameHost, vm: &mut Vm) {
    if let Some(l) = vm.level {
        apply(vm, l, now());
    }
    host.add_frame_hook("clock", |vm, _dt| {
        if let Some(l) = vm.level {
            apply(vm, l, now());
        }
    });
}

/// Keeps every level's LevelInfo clock current. Call before the first level boots.
pub fn install() {
    crate::host::register_level_hook(level_hook);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_from_unix() {
        // 2026-10-04 01:58:30 UTC is a Sunday.
        let c = from_unix(1_791_079_110, 7);
        assert_eq!((c.year, c.month, c.day, c.hour, c.minute, c.second, c.millisecond), (2026, 10, 4, 1, 58, 30, 7));
        assert_eq!(c.day_of_week, 0);
        let e = from_unix(0, 0);
        assert_eq!((e.year, e.month, e.day, e.day_of_week), (1970, 1, 1, 4));
    }

    #[test]
    fn local_clock_is_sane() {
        let c = now();
        assert!(c.year >= 2024 && (1..=12).contains(&c.month) && (1..=31).contains(&c.day) && c.hour < 24);
    }
}
