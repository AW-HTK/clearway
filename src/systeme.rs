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

#[cfg(unix)]
pub fn heure_locale(secs: u64) -> HeureLocale {
    let t = secs as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&t, &mut tm) };
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
        FileTimeToSystemTime(&ft, &mut utc);
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
