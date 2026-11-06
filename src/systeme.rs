use std::time::{SystemTime, UNIX_EPOCH};

pub fn maintenant() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub struct HeureLocale {
    pub annee: i32,
    pub mois: u32,
    pub jour: u32,
    pub heure: u32,
    pub minute: u32,
    pub seconde: u32,
}

/// Heure UTC calculée sans le système : repli quand la conversion locale échoue.
pub fn heure_utc(secs: u64) -> HeureLocale {
    let jours = (secs / 86_400) as i64;
    let reste = secs % 86_400;
    // Conversion jours → date civile (algorithme de Howard Hinnant).
    let z = jours + 719_468;
    let ere = z.div_euclid(146_097);
    let jde = z - ere * 146_097;
    let ade = (jde - jde / 1460 + jde / 36_524 - jde / 146_096) / 365;
    let jda = jde - (365 * ade + ade / 4 - ade / 100);
    let mp = (5 * jda + 2) / 153;
    let jour = (jda - (153 * mp + 2) / 5 + 1) as u32;
    let mois = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let annee = (ade + ere * 400 + i64::from(mois <= 2)) as i32;
    HeureLocale {
        annee,
        mois,
        jour,
        heure: (reste / 3600) as u32,
        minute: (reste % 3600 / 60) as u32,
        seconde: (reste % 60) as u32,
    }
}

#[cfg(unix)]
pub fn heure_locale(secs: u64) -> HeureLocale {
    let t = secs as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        return heure_utc(secs);
    }
    HeureLocale {
        annee: tm.tm_year + 1900,
        mois: (tm.tm_mon + 1) as u32,
        jour: tm.tm_mday as u32,
        heure: tm.tm_hour as u32,
        minute: tm.tm_min as u32,
        seconde: tm.tm_sec as u32,
    }
}

#[cfg(windows)]
pub fn heure_locale(secs: u64) -> HeureLocale {
    use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows_sys::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
    let intervalles = (secs + 11_644_473_600) * 10_000_000;
    let ft = FILETIME {
        dwLowDateTime: intervalles as u32,
        dwHighDateTime: (intervalles >> 32) as u32,
    };
    let mut utc: SYSTEMTIME = unsafe { std::mem::zeroed() };
    let mut local: SYSTEMTIME = unsafe { std::mem::zeroed() };
    unsafe {
        if FileTimeToSystemTime(&ft, &mut utc) == 0 {
            return heure_utc(secs);
        }
        if SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local) == 0 {
            local = utc;
        }
    }
    HeureLocale {
        annee: local.wYear as i32,
        mois: local.wMonth as u32,
        jour: local.wDay as u32,
        heure: local.wHour as u32,
        minute: local.wMinute as u32,
        seconde: local.wSecond as u32,
    }
}

pub fn hhmm(secs: u64) -> String {
    let h = heure_locale(secs);
    format!("{:02}:{:02}", h.heure, h.minute)
}

pub fn horodatage(secs: u64) -> String {
    let h = heure_locale(secs);
    format!(
        "{}-{:02}-{:02} {:02}:{:02}:{:02}",
        h.annee, h.mois, h.jour, h.heure, h.minute, h.seconde
    )
}

#[cfg(unix)]
pub fn aleatoire(n: usize) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut buf = vec![0u8; n];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut buf)?;
    Ok(buf)
}

#[cfg(windows)]
pub fn aleatoire(n: usize) -> std::io::Result<Vec<u8>> {
    use windows_sys::Win32::Security::Cryptography::{BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom};
    let mut buf = vec![0u8; n];
    let statut = unsafe {
        BCryptGenRandom(
            std::ptr::null_mut(),
            buf.as_mut_ptr(),
            n as u32,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    if statut != 0 {
        return Err(std::io::Error::other(format!("BCryptGenRandom : {statut:#x}")));
    }
    Ok(buf)
}

pub fn hex(octets: &[u8]) -> String {
    octets.iter().map(|o| format!("{o:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heure_utc_calculee() {
        let date = |s| {
            let h = heure_utc(s);
            (h.annee, h.mois, h.jour, h.heure, h.minute, h.seconde)
        };
        assert_eq!(date(0), (1970, 1, 1, 0, 0, 0));
        assert_eq!(date(951_782_400), (2000, 2, 29, 0, 0, 0));
        assert_eq!(date(1_793_921_480), (2026, 11, 5, 23, 31, 20));
        assert_eq!(date(4_102_444_799), (2099, 12, 31, 23, 59, 59));
    }
}
