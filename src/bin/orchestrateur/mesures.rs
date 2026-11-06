use std::path::Path;

use commun::modele::Mesures;

#[cfg(windows)]
pub use windows::Capteur;

#[cfg(not(windows))]
pub use autre::Capteur;

#[cfg(windows)]
mod windows {
    use std::path::Path;

    use commun::modele::{Mesures, Vmmem};
    use windows_sys::Wdk::System::SystemInformation::{NtQuerySystemInformation, SystemProcessInformation};
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::ProcessStatus::{GetPerformanceInfo, PERFORMANCE_INFORMATION};
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    use windows_sys::Win32::System::Threading::GetSystemTimes;
    use windows_sys::Win32::System::WindowsProgramming::SYSTEM_PROCESS_INFORMATION;

    const MIO: u64 = 1024 * 1024;
    const STATUS_INFO_LENGTH_MISMATCH: i32 = 0xC000_0004_u32 as i32;

    pub struct Capteur {
        cpu: Option<(u64, u64)>,
        tampon: Vec<u64>,
        vmmem: Option<Vmmem>,
    }

    fn valeur(ft: &FILETIME) -> u64 {
        ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64
    }

    impl Capteur {
        pub fn nouveau(_dossier: &Path) -> Capteur {
            Capteur { cpu: None, tampon: vec![0; 32 * 1024], vmmem: None }
        }

        /// vmmemWSL refuse OpenProcess même en lecture limitée : sa mémoire résidente ne se lit que dans la
        /// liste système des processus, celle qu'utilise Get-Process.
        fn lire_vmmem(&mut self) -> Option<Vmmem> {
            loop {
                let mut requis = 0u32;
                let statut = unsafe {
                    NtQuerySystemInformation(
                        SystemProcessInformation,
                        self.tampon.as_mut_ptr().cast(),
                        (self.tampon.len() * 8) as u32,
                        &mut requis,
                    )
                };
                if statut == STATUS_INFO_LENGTH_MISMATCH && self.tampon.len() < 4 * 1024 * 1024 {
                    self.tampon.resize((requis as usize + 64 * 1024) / 8, 0);
                    continue;
                }
                if statut < 0 {
                    return self.vmmem;
                }
                break;
            }
            let base = self.tampon.as_ptr() as *const u8;
            let mut decalage = 0usize;
            self.vmmem = loop {
                let p = unsafe { &*(base.add(decalage) as *const SYSTEM_PROCESS_INFORMATION) };
                let longueur = p.ImageName.Length as usize / 2;
                if !p.ImageName.Buffer.is_null() && longueur > 0 {
                    let nom = unsafe { std::slice::from_raw_parts(p.ImageName.Buffer, longueur) };
                    if String::from_utf16_lossy(nom).eq_ignore_ascii_case("vmmemWSL") {
                        break Some(Vmmem { pid: p.UniqueProcessId as usize as u32, rss_mo: p.WorkingSetSize as u64 / MIO });
                    }
                }
                if p.NextEntryOffset == 0 {
                    break None;
                }
                decalage += p.NextEntryOffset as usize;
            };
            self.vmmem
        }

        fn lire_cpu(&mut self) -> f32 {
            let mut inactif: FILETIME = unsafe { std::mem::zeroed() };
            let mut noyau: FILETIME = unsafe { std::mem::zeroed() };
            let mut utilisateur: FILETIME = unsafe { std::mem::zeroed() };
            if unsafe { GetSystemTimes(&mut inactif, &mut noyau, &mut utilisateur) } == 0 {
                return 0.0;
            }
            // Le temps noyau inclut le temps inactif.
            let total = valeur(&noyau) + valeur(&utilisateur);
            let inactif = valeur(&inactif);
            let pct = match self.cpu {
                Some((t0, i0)) if total > t0 => 100.0 * (1.0 - (inactif - i0) as f64 / (total - t0) as f64),
                _ => 0.0,
            };
            self.cpu = Some((total, inactif));
            pct.clamp(0.0, 100.0) as f32
        }

        pub fn lire(&mut self) -> Mesures {
            let mut memoire: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
            memoire.dwLength = size_of::<MEMORYSTATUSEX>() as u32;
            unsafe { GlobalMemoryStatusEx(&mut memoire) };
            let mut perf: PERFORMANCE_INFORMATION = unsafe { std::mem::zeroed() };
            perf.cb = size_of::<PERFORMANCE_INFORMATION>() as u32;
            let perf_ok = unsafe { GetPerformanceInfo(&mut perf, perf.cb) } != 0;
            let page = perf.PageSize as u64;
            Mesures {
                ram_totale_mo: memoire.ullTotalPhys / MIO,
                ram_dispo_mo: memoire.ullAvailPhys / MIO,
                commit_mo: if perf_ok { perf.CommitTotal as u64 * page / MIO } else { 0 },
                commit_limite_mo: if perf_ok { perf.CommitLimit as u64 * page / MIO } else { 0 },
                cpu_pct: self.lire_cpu(),
                coeurs_windows: std::thread::available_parallelism().map_or(0, |n| n.get() as u32),
                swap_sortie_ps: 0.0,
                vmmem: self.lire_vmmem(),
                wsl: None,
            }
        }
    }
}

#[cfg(not(windows))]
mod autre {
    use std::path::{Path, PathBuf};

    use commun::modele::{Mesures, Vmmem};

    /// Hors Windows (tests) : mesures fixes, remplaçables par un fichier `mesures-test.json` du dossier d'état.
    pub struct Capteur {
        fichier: PathBuf,
    }

    impl Capteur {
        pub fn nouveau(dossier: &Path) -> Capteur {
            Capteur { fichier: dossier.join("mesures-test.json") }
        }

        pub fn lire(&mut self) -> Mesures {
            std::fs::read(&self.fichier)
                .ok()
                .and_then(|o| serde_json::from_slice(&o).ok())
                .unwrap_or(Mesures {
                    ram_totale_mo: 32 * 1024,
                    ram_dispo_mo: 20 * 1024,
                    commit_mo: 0,
                    commit_limite_mo: 0,
                    cpu_pct: 0.0,
                    coeurs_windows: 16,
                    swap_sortie_ps: 0.0,
                    vmmem: Some(Vmmem { pid: 4242, rss_mo: 4096 }),
                    wsl: None,
                })
        }
    }
}

pub fn premieres(dossier: &Path) -> (Capteur, Mesures) {
    let mut c = Capteur::nouveau(dossier);
    let m = c.lire();
    (c, m)
}
