use std::time::{SystemTime, UNIX_EPOCH};

pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub struct LocalTime {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

/// UTC time computed without the OS: fallback when the local conversion fails.
pub fn utc_time(secs: u64) -> LocalTime {
    let days = (secs / 86_400) as i64;
    let left = secs % 86_400;
    // Days to civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = (yoe + era * 400 + i64::from(month <= 2)) as i32;
    LocalTime {
        year,
        month,
        day,
        hour: (left / 3600) as u32,
        minute: (left % 3600 / 60) as u32,
        second: (left % 60) as u32,
    }
}

#[cfg(unix)]
pub fn local_time(secs: u64) -> LocalTime {
    let t = secs as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        return utc_time(secs);
    }
    LocalTime {
        year: tm.tm_year + 1900,
        month: (tm.tm_mon + 1) as u32,
        day: tm.tm_mday as u32,
        hour: tm.tm_hour as u32,
        minute: tm.tm_min as u32,
        second: tm.tm_sec as u32,
    }
}

#[cfg(windows)]
pub fn local_time(secs: u64) -> LocalTime {
    use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows_sys::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
    let intervals = (secs + 11_644_473_600) * 10_000_000;
    let ft = FILETIME {
        dwLowDateTime: intervals as u32,
        dwHighDateTime: (intervals >> 32) as u32,
    };
    let mut utc: SYSTEMTIME = unsafe { std::mem::zeroed() };
    let mut local: SYSTEMTIME = unsafe { std::mem::zeroed() };
    unsafe {
        if FileTimeToSystemTime(&ft, &mut utc) == 0 {
            return utc_time(secs);
        }
        if SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local) == 0 {
            local = utc;
        }
    }
    LocalTime {
        year: local.wYear as i32,
        month: local.wMonth as u32,
        day: local.wDay as u32,
        hour: local.wHour as u32,
        minute: local.wMinute as u32,
        second: local.wSecond as u32,
    }
}

pub fn hhmm(secs: u64) -> String {
    let h = local_time(secs);
    format!("{:02}:{:02}", h.hour, h.minute)
}

pub fn timestamp(secs: u64) -> String {
    let h = local_time(secs);
    format!(
        "{}-{:02}-{:02} {:02}:{:02}:{:02}",
        h.year, h.month, h.day, h.hour, h.minute, h.second
    )
}

#[cfg(unix)]
pub fn random(n: usize) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut buf = vec![0u8; n];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut buf)?;
    Ok(buf)
}

#[cfg(windows)]
pub fn random(n: usize) -> std::io::Result<Vec<u8>> {
    use windows_sys::Win32::Security::Cryptography::{BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom};
    let mut buf = vec![0u8; n];
    let status = unsafe {
        BCryptGenRandom(
            std::ptr::null_mut(),
            buf.as_mut_ptr(),
            n as u32,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    if status != 0 {
        return Err(std::io::Error::other(format!("BCryptGenRandom: {status:#x}")));
    }
    Ok(buf)
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|o| format!("{o:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_time_is_computed() {
        let date = |s| {
            let h = utc_time(s);
            (h.year, h.month, h.day, h.hour, h.minute, h.second)
        };
        assert_eq!(date(0), (1970, 1, 1, 0, 0, 0));
        assert_eq!(date(951_782_400), (2000, 2, 29, 0, 0, 0));
        assert_eq!(date(1_793_921_480), (2026, 11, 5, 23, 31, 20));
        assert_eq!(date(4_102_444_799), (2099, 12, 31, 23, 59, 59));
    }
}
