use std::path::Path;

use commun::modele::Mesures;

#[cfg(windows)]
pub use windows::Capteur;

#[cfg(not(windows))]
pub use autre::Capteur;

/// Place des champs utiles dans une entrée de la liste système des processus (`SYSTEM_PROCESS_INFORMATION`).
#[derive(Clone, Copy)]
#[cfg_attr(not(windows), allow(dead_code))]
pub struct Disposition {
    pub taille: usize,
    pub suivant: usize,
    pub nom_longueur: usize,
    pub nom_tampon: usize,
    pub pid: usize,
    pub memoire: usize,
}

/// Cherche un processus par nom dans la liste rendue par le noyau, sans jamais lire hors des `octets` remplis ;
/// `adresse` est celle du premier octet, car les noms y sont désignés par des pointeurs absolus.
/// Rend le PID et la mémoire résidente en octets.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn chercher_processus(octets: &[u8], adresse: usize, d: &Disposition, nom: &str) -> Option<(u32, u64)> {
    let lire = |debut: usize, n: usize| -> Option<u64> {
        let champ = octets.get(debut..debut.checked_add(n)?)?;
        let mut tampon = [0u8; 8];
        tampon[..n].copy_from_slice(champ);
        Some(u64::from_le_bytes(tampon))
    };
    let pointeur = size_of::<usize>();
    let mut entree = 0usize;
    loop {
        if entree.checked_add(d.taille)? > octets.len() {
            return None;
        }
        let longueur = lire(entree + d.nom_longueur, 2)? as usize;
        let tampon = lire(entree + d.nom_tampon, pointeur)? as usize;
        let texte = tampon
            .checked_sub(adresse)
            .filter(|_| longueur > 0 && longueur.is_multiple_of(2))
            .and_then(|debut| octets.get(debut..debut.checked_add(longueur)?));
        if let Some(texte) = texte {
            let unites: Vec<u16> = texte.as_chunks::<2>().0.iter().map(|&c| u16::from_le_bytes(c)).collect();
            if String::from_utf16_lossy(&unites).eq_ignore_ascii_case(nom) {
                return Some((lire(entree + d.pid, pointeur)? as u32, lire(entree + d.memoire, pointeur)?));
            }
        }
        let suivant = lire(entree + d.suivant, 4)? as usize;
        if suivant == 0 {
            return None;
        }
        entree = entree.checked_add(suivant)?;
    }
}

#[cfg(windows)]
mod windows {
    use std::mem::offset_of;
    use std::path::{Path, PathBuf};

    use commun::modele::{Mesures, Vmmem};
    use windows_sys::Wdk::System::SystemInformation::{NtQuerySystemInformation, SystemProcessInformation};
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::ProcessStatus::{GetPerformanceInfo, PERFORMANCE_INFORMATION};
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    use windows_sys::Win32::System::Threading::GetSystemTimes;
    use windows_sys::Win32::System::WindowsProgramming::SYSTEM_PROCESS_INFORMATION;

    const MIO: u64 = 1024 * 1024;
    const STATUS_INFO_LENGTH_MISMATCH: i32 = 0xC000_0004_u32 as i32;
    const TAMPON_MAX_OCTETS: usize = 32 * 1024 * 1024;

    const DISPOSITION: super::Disposition = super::Disposition {
        taille: size_of::<SYSTEM_PROCESS_INFORMATION>(),
        suivant: offset_of!(SYSTEM_PROCESS_INFORMATION, NextEntryOffset),
        nom_longueur: offset_of!(SYSTEM_PROCESS_INFORMATION, ImageName.Length),
        nom_tampon: offset_of!(SYSTEM_PROCESS_INFORMATION, ImageName.Buffer),
        pid: offset_of!(SYSTEM_PROCESS_INFORMATION, UniqueProcessId),
        memoire: offset_of!(SYSTEM_PROCESS_INFORMATION, WorkingSetSize),
    };

    pub struct Capteur {
        cpu: Option<(u64, u64)>,
        tampon: Vec<u64>,
        vmmem: Option<Vmmem>,
        memoire: Option<(u64, u64)>,
        dossier: PathBuf,
        signale: bool,
    }

    fn valeur(ft: &FILETIME) -> u64 {
        ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64
    }

    impl Capteur {
        pub fn nouveau(dossier: &Path) -> Capteur {
            Capteur {
                cpu: None,
                tampon: vec![0; 32 * 1024],
                vmmem: None,
                memoire: None,
                dossier: dossier.to_path_buf(),
                signale: false,
            }
        }

        fn signaler(&mut self, texte: &str) {
            if !self.signale {
                self.signale = true;
                crate::serveur::journaliser(&self.dossier, &format!("ERREUR mesures : {texte} ; dernières valeurs gardées."));
            }
        }

        /// vmmemWSL refuse OpenProcess même en lecture limitée : sa mémoire résidente ne se lit que dans la
        /// liste système des processus, celle qu'utilise Get-Process.
        fn lire_vmmem(&mut self) -> Option<Vmmem> {
            let rempli = loop {
                let capacite = self.tampon.len() * 8;
                let mut requis = 0u32;
                let statut = unsafe {
                    NtQuerySystemInformation(SystemProcessInformation, self.tampon.as_mut_ptr().cast(), capacite as u32, &mut requis)
                };
                if statut == STATUS_INFO_LENGTH_MISMATCH {
                    let voulu = (requis as usize).saturating_add(64 * 1024).min(TAMPON_MAX_OCTETS);
                    if voulu > capacite {
                        self.tampon.resize(voulu.div_ceil(8), 0);
                        continue;
                    }
                }
                if statut < 0 {
                    return self.vmmem;
                }
                break if requis == 0 { capacite } else { (requis as usize).min(capacite) };
            };
            // Vec<u64> : alignement suffisant pour le noyau ; on ne relit que les octets qu'il a remplis.
            let octets = unsafe { std::slice::from_raw_parts(self.tampon.as_ptr().cast::<u8>(), rempli) };
            self.vmmem = super::chercher_processus(octets, octets.as_ptr() as usize, &DISPOSITION, "vmmemWSL")
                .map(|(pid, octets)| Vmmem { pid, rss_mo: octets / MIO });
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
            if unsafe { GlobalMemoryStatusEx(&mut memoire) } != 0 {
                self.memoire = Some((memoire.ullTotalPhys / MIO, memoire.ullAvailPhys / MIO));
            } else {
                let e = std::io::Error::last_os_error();
                self.signaler(&format!("GlobalMemoryStatusEx a échoué ({e})"));
            }
            let (ram_totale_mo, ram_dispo_mo) = self.memoire.unwrap_or((0, 0));
            let mut perf: PERFORMANCE_INFORMATION = unsafe { std::mem::zeroed() };
            perf.cb = size_of::<PERFORMANCE_INFORMATION>() as u32;
            let perf_ok = unsafe { GetPerformanceInfo(&mut perf, perf.cb) } != 0;
            let page = perf.PageSize as u64;
            Mesures {
                ram_totale_mo,
                ram_dispo_mo,
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

#[cfg(test)]
mod tests {
    use super::*;

    const D: Disposition = Disposition { taille: 48, suivant: 0, nom_longueur: 8, nom_tampon: 16, pid: 24, memoire: 32 };

    /// Liste factice : une entrée par (nom, pid, mémoire, décalage vers la suivante), noms rangés après les entrées.
    fn liste(entrees: &[(&str, u64, u64, u32)]) -> Vec<u8> {
        let mut octets = vec![0u8; 4096];
        let adresse = octets.as_ptr() as usize;
        let mut nom_ici = 2048;
        let mut entree = 0;
        for &(nom, pid, memoire, suivant) in entrees {
            let utf16: Vec<u8> = nom.encode_utf16().flat_map(u16::to_le_bytes).collect();
            octets[nom_ici..nom_ici + utf16.len()].copy_from_slice(&utf16);
            octets[entree + D.suivant..entree + D.suivant + 4].copy_from_slice(&suivant.to_le_bytes());
            octets[entree + D.nom_longueur..entree + D.nom_longueur + 2].copy_from_slice(&(utf16.len() as u16).to_le_bytes());
            octets[entree + D.nom_tampon..entree + D.nom_tampon + 8].copy_from_slice(&((adresse + nom_ici) as u64).to_le_bytes());
            octets[entree + D.pid..entree + D.pid + 8].copy_from_slice(&pid.to_le_bytes());
            octets[entree + D.memoire..entree + D.memoire + 8].copy_from_slice(&memoire.to_le_bytes());
            nom_ici += 128;
            entree += suivant as usize;
        }
        octets
    }

    fn chercher(octets: &[u8], longueur: usize) -> Option<(u32, u64)> {
        chercher_processus(&octets[..longueur], octets.as_ptr() as usize, &D, "vmmemWSL")
    }

    #[test]
    fn trouve_vmmem_dans_une_liste_saine() {
        let o = liste(&[("System", 4, 1, 64), ("explorer.exe", 900, 2, 64), ("VmmemWSL", 4242, 8 << 30, 0)]);
        assert_eq!(chercher(&o, o.len()), Some((4242, 8 << 30)));
        let o = liste(&[("System", 4, 1, 64), ("explorer.exe", 900, 2, 0)]);
        assert_eq!(chercher(&o, o.len()), None);
    }

    #[test]
    fn liste_incoherente_sans_lecture_hors_limites() {
        // Entrée suivante au-delà de ce que le noyau a rempli.
        let o = liste(&[("System", 4, 1, 4000), ("vmmemWSL", 4242, 1, 0)]);
        assert_eq!(chercher(&o, 1024), None);
        // Remplissage tronqué au milieu de la première entrée.
        let o = liste(&[("vmmemWSL", 4242, 1, 0)]);
        assert_eq!(chercher(&o, D.taille - 1), None);
        // Nom hors du tampon rempli : entrée ignorée, la suivante est lue.
        let o = liste(&[("vmmemWSL", 1, 1, 64), ("vmmemWSL", 4242, 7, 0)]);
        assert_eq!(chercher(&o, 2048 + 8), None);
        let mut o = liste(&[("autre", 1, 1, 64), ("vmmemWSL", 4242, 7, 0)]);
        o[D.nom_tampon..D.nom_tampon + 8].copy_from_slice(&u64::MAX.to_le_bytes());
        assert_eq!(chercher(&o, o.len()), Some((4242, 7)));
        // Longueur de nom impaire ou démesurée.
        let mut o = liste(&[("vmmemWSL", 4242, 7, 0)]);
        o[D.nom_longueur..D.nom_longueur + 2].copy_from_slice(&15u16.to_le_bytes());
        assert_eq!(chercher(&o, o.len()), None);
        o[D.nom_longueur..D.nom_longueur + 2].copy_from_slice(&u16::MAX.to_le_bytes());
        assert_eq!(chercher(&o, o.len()), None);
        // Décalage énorme : pas de débordement d'addition.
        let o = liste(&[("System", 4, 1, u32::MAX)]);
        assert_eq!(chercher(&o, o.len()), None);
    }
}
