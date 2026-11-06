use std::io::{self, BufRead, BufReader, Read, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use commun::protocole::{RELAIS_PERDU, lire_ligne};

use crate::serveur::{self, Partage};

pub use imp::{Ecoute, ecouter, instance_unique, relais};

const MAX_CONNEXIONS: usize = 64;

/// Requêtes sans `id` envoyées et réponses sans `id` reçues : à la fin de stdin, le relais rend encore celles-là.
#[derive(Default)]
struct Sequentielles {
    envoyees: u64,
    recues: u64,
    tube_ferme: bool,
}

fn sans_id(ligne: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(ligne).map_or(true, |v| v.get("id").is_none_or(|i| i.is_null()))
}

/// Relaie dans les deux sens à la fois : lignes de stdin vers le tube, lignes du tube vers stdout dès qu'elles arrivent,
/// ce qui permet plusieurs requêtes `id` en cours. À la fin de stdin, le relais attend les réponses aux requêtes sans `id`
/// (`echo … | relais`), mais pas celles des requêtes `id` : un client multiplexé qui part doit fermer sa liaison aussitôt.
fn pomper(tube_lecture: impl Read + Send + 'static, mut tube_ecriture: impl Write) -> i32 {
    let etat = Arc::new((Mutex::new(Sequentielles::default()), Condvar::new()));
    {
        let etat = etat.clone();
        std::thread::spawn(move || {
            let mut retour = BufReader::new(tube_lecture);
            let mut sortie = io::stdout();
            while let Ok(Some(r)) = lire_ligne(&mut retour) {
                if writeln!(sortie, "{r}").and_then(|_| sortie.flush()).is_err() {
                    std::process::exit(0);
                }
                if sans_id(&r) {
                    etat.0.lock().unwrap_or_else(|e| e.into_inner()).recues += 1;
                    etat.1.notify_all();
                }
            }
            etat.0.lock().unwrap_or_else(|e| e.into_inner()).tube_ferme = true;
            etat.1.notify_all();
            eprintln!("relais : l'orchestrateur a fermé le tube.");
            std::process::exit(RELAIS_PERDU);
        });
    }
    let mut entree = io::stdin().lock();
    loop {
        let mut ligne = String::new();
        match entree.read_line(&mut ligne) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        if !ligne.ends_with('\n') {
            ligne.push('\n');
        }
        if !ligne.trim().is_empty() && sans_id(&ligne) {
            etat.0.lock().unwrap_or_else(|e| e.into_inner()).envoyees += 1;
        }
        if tube_ecriture.write_all(ligne.as_bytes()).and_then(|_| tube_ecriture.flush()).is_err() {
            eprintln!("relais : l'orchestrateur a fermé le tube.");
            return RELAIS_PERDU;
        }
    }
    let (verrou, signal) = &*etat;
    let mut e = verrou.lock().unwrap_or_else(|e| e.into_inner());
    while e.recues < e.envoyees && !e.tube_ferme {
        e = signal.wait(e).unwrap_or_else(|e| e.into_inner());
    }
    0
}

struct Compteur(Arc<AtomicUsize>);

impl Drop for Compteur {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

pub fn servir(mut ecoute: Ecoute, p: Arc<Partage>) {
    let actives = Arc::new(AtomicUsize::new(0));
    loop {
        let flux = match ecoute.accepter() {
            Ok(f) => f,
            Err(_) => {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
        };
        if actives.fetch_add(1, Ordering::SeqCst) >= MAX_CONNEXIONS {
            actives.fetch_sub(1, Ordering::SeqCst);
            continue;
        }
        let garde = Compteur(actives.clone());
        let p = p.clone();
        let flux = Arc::new(flux);
        let lance = std::thread::Builder::new().stack_size(256 * 1024).spawn(move || {
            let _garde = garde;
            if let Ok((lecture, ecriture)) = flux.parties() {
                let sonde = flux.clone();
                serveur::servir_connexion(&p, lecture, ecriture, serveur::Contexte::nouveau(move || sonde.vivant()));
            }
        });
        if lance.is_err() {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::io::{self, Read, Write};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use commun::protocole::{RELAIS_ABSENT, RELAIS_PERDU};
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, ERROR_BROKEN_PIPE, ERROR_FILE_NOT_FOUND, ERROR_IO_PENDING,
        ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, ERROR_PIPE_NOT_CONNECTED, GENERIC_READ, GENERIC_WRITE, GetLastError,
        HANDLE, INVALID_HANDLE_VALUE, LocalFree,
    };
    use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_OVERLAPPED, ReadFile, WriteFile};
    use windows_sys::Win32::System::IO::{GetOverlappedResult, OVERLAPPED};
    use windows_sys::Win32::System::Threading::CreateEventW;
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser};
    use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, OPEN_EXISTING, PIPE_ACCESS_DUPLEX};
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
        PIPE_UNLIMITED_INSTANCES, PIPE_WAIT, PeekNamedPipe, WaitNamedPipeW,
    };
    use windows_sys::Win32::System::Threading::{CreateMutexW, GetCurrentProcess, OpenProcessToken};

    /// Tube ouvert en E/S asynchrone : sur un handle synchrone, une lecture en cours bloquerait les écritures des autres fils.
    pub struct Tube(HANDLE);

    // Chaque opération a sa propre structure OVERLAPPED et son propre événement.
    unsafe impl Send for Tube {}
    unsafe impl Sync for Tube {}

    impl Drop for Tube {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
    }

    impl Tube {
        /// Lance une opération asynchrone et attend sa fin.
        fn operation(&self, lancer: impl FnOnce(*mut OVERLAPPED) -> i32) -> io::Result<usize> {
            unsafe {
                let evenement = CreateEventW(std::ptr::null(), 1, 0, std::ptr::null());
                if evenement.is_null() {
                    return Err(io::Error::last_os_error());
                }
                let mut ov: OVERLAPPED = std::mem::zeroed();
                ov.hEvent = evenement;
                let mut n = 0u32;
                let resultat = if lancer(&mut ov) != 0 || GetLastError() == ERROR_IO_PENDING {
                    if GetOverlappedResult(self.0, &ov, &mut n, 1) != 0 { Ok(n as usize) } else { Err(io::Error::last_os_error()) }
                } else {
                    Err(io::Error::last_os_error())
                };
                CloseHandle(evenement);
                resultat
            }
        }

        fn lire(&self, tampon: &mut [u8]) -> io::Result<usize> {
            let longueur = tampon.len().min(u32::MAX as usize) as u32;
            let h = self.0;
            match self.operation(|ov| unsafe { ReadFile(h, tampon.as_mut_ptr(), longueur, std::ptr::null_mut(), ov) }) {
                Err(e) if [ERROR_BROKEN_PIPE, ERROR_PIPE_NOT_CONNECTED].map(|c| c as i32).contains(&e.raw_os_error().unwrap_or(0)) => Ok(0),
                r => r,
            }
        }

        fn ecrire(&self, tampon: &[u8]) -> io::Result<usize> {
            let longueur = tampon.len().min(u32::MAX as usize) as u32;
            let h = self.0;
            self.operation(|ov| unsafe { WriteFile(h, tampon.as_ptr(), longueur, std::ptr::null_mut(), ov) })
        }
    }

    pub struct Lecteur(Arc<Tube>);
    pub struct Ecrivain(Arc<Tube>);

    impl Read for Lecteur {
        fn read(&mut self, tampon: &mut [u8]) -> io::Result<usize> {
            self.0.lire(tampon)
        }
    }

    impl Write for Ecrivain {
        fn write(&mut self, tampon: &[u8]) -> io::Result<usize> {
            self.0.ecrire(tampon)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn large(s: &str) -> Vec<u16> {
        s.encode_utf16().chain([0]).collect()
    }

    fn nom(tube: &str) -> Vec<u16> {
        large(&format!(r"\\.\pipe\{tube}"))
    }

    fn sid_utilisateur() -> io::Result<String> {
        unsafe {
            let mut jeton: HANDLE = std::ptr::null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut jeton) == 0 {
                return Err(io::Error::last_os_error());
            }
            let mut tampon = vec![0u64; 64];
            let mut taille = 0u32;
            let ok = GetTokenInformation(jeton, TokenUser, tampon.as_mut_ptr().cast(), (tampon.len() * 8) as u32, &mut taille);
            let erreur = io::Error::last_os_error();
            CloseHandle(jeton);
            if ok == 0 {
                return Err(erreur);
            }
            let utilisateur = &*(tampon.as_ptr() as *const TOKEN_USER);
            let mut texte: *mut u16 = std::ptr::null_mut();
            if ConvertSidToStringSidW(utilisateur.User.Sid, &mut texte) == 0 {
                return Err(io::Error::last_os_error());
            }
            let longueur = (0..).take_while(|&i| *texte.add(i) != 0).count();
            let sid = String::from_utf16_lossy(std::slice::from_raw_parts(texte, longueur));
            LocalFree(texte.cast());
            Ok(sid)
        }
    }

    /// Mutex nommé par tube : une seule instance par tube, mais une instance de test peut tourner à côté.
    pub fn instance_unique(tube: &str) -> Result<HANDLE, ()> {
        let nom = large(&format!("Local\\orchestrateur-charge-{tube}"));
        unsafe {
            let h = CreateMutexW(std::ptr::null(), 1, nom.as_ptr());
            if h.is_null() || GetLastError() == ERROR_ALREADY_EXISTS {
                if !h.is_null() {
                    CloseHandle(h);
                }
                return Err(());
            }
            Ok(h)
        }
    }

    pub struct Ecoute {
        nom: Vec<u16>,
        securite: PSECURITY_DESCRIPTOR,
        suivante: Option<HANDLE>,
    }

    // Le descripteur et les handles ne servent qu'au fil d'écoute.
    unsafe impl Send for Ecoute {}

    impl Drop for Ecoute {
        fn drop(&mut self) {
            unsafe {
                if let Some(h) = self.suivante.take() {
                    CloseHandle(h);
                }
                if !self.securite.is_null() {
                    LocalFree(self.securite.cast());
                }
            }
        }
    }

    impl Ecoute {
        fn creer(&self, premiere: bool) -> io::Result<HANDLE> {
            let attributs = SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: self.securite,
                bInheritHandle: 0,
            };
            let h = unsafe {
                CreateNamedPipeW(
                    self.nom.as_ptr(),
                    PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | if premiere { FILE_FLAG_FIRST_PIPE_INSTANCE } else { 0 },
                    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                    PIPE_UNLIMITED_INSTANCES,
                    64 * 1024,
                    64 * 1024,
                    0,
                    &attributs,
                )
            };
            if h == INVALID_HANDLE_VALUE {
                let e = unsafe { GetLastError() };
                return Err(if e == ERROR_ACCESS_DENIED {
                    io::Error::new(io::ErrorKind::AddrInUse, "un autre programme a déjà créé ce tube")
                } else {
                    io::Error::from_raw_os_error(e as i32)
                });
            }
            Ok(h)
        }

        pub fn accepter(&mut self) -> io::Result<Flux> {
            let h = match self.suivante.take() {
                Some(h) => h,
                None => self.creer(false)?,
            };
            let tube = Tube(h);
            // Client déjà connecté entre la création et l'attente : l'événement ne sera jamais signalé.
            let connecte = unsafe {
                let evenement = CreateEventW(std::ptr::null(), 1, 0, std::ptr::null());
                if evenement.is_null() {
                    return Err(io::Error::last_os_error());
                }
                let mut ov: OVERLAPPED = std::mem::zeroed();
                ov.hEvent = evenement;
                let mut n = 0u32;
                let r = if ConnectNamedPipe(h, &mut ov) != 0 {
                    Ok(())
                } else {
                    match GetLastError() {
                        ERROR_PIPE_CONNECTED => Ok(()),
                        ERROR_IO_PENDING if GetOverlappedResult(h, &ov, &mut n, 1) != 0 => Ok(()),
                        ERROR_IO_PENDING => Err(io::Error::last_os_error()),
                        e => Err(io::Error::from_raw_os_error(e as i32)),
                    }
                };
                CloseHandle(evenement);
                r
            };
            connecte.map(|()| Flux { tube: Arc::new(tube) })
        }
    }

    /// Tube réservé au compte courant (et à SYSTEM), refusé aux clients distants.
    pub fn ecouter(tube: &str) -> io::Result<Ecoute> {
        let sddl = large(&format!("D:P(A;;GA;;;SY)(A;;GA;;;{})", sid_utilisateur()?));
        let mut securite: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), SDDL_REVISION_1, &mut securite, std::ptr::null_mut())
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let mut e = Ecoute { nom: nom(tube), securite, suivante: None };
        e.suivante = Some(e.creer(true)?);
        Ok(e)
    }

    pub struct Flux {
        tube: Arc<Tube>,
    }

    impl Flux {
        pub fn parties(&self) -> io::Result<(Box<dyn Read + Send>, Box<dyn Write + Send>)> {
            Ok((Box::new(Lecteur(self.tube.clone())), Box::new(Ecrivain(self.tube.clone()))))
        }

        pub fn vivant(&self) -> bool {
            let mut disponible = 0u32;
            unsafe {
                PeekNamedPipe(
                    self.tube.0,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null_mut(),
                    &mut disponible,
                    std::ptr::null_mut(),
                ) != 0
            }
        }
    }

    pub fn relais(tube: &str) -> i32 {
        let nom = nom(tube);
        let debut = Instant::now();
        let h = loop {
            let h = unsafe {
                CreateFileW(
                    nom.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    0,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    FILE_FLAG_OVERLAPPED,
                    std::ptr::null_mut(),
                )
            };
            if h != INVALID_HANDLE_VALUE {
                break h;
            }
            match unsafe { GetLastError() } {
                ERROR_PIPE_BUSY if debut.elapsed() < Duration::from_secs(10) => unsafe {
                    WaitNamedPipeW(nom.as_ptr(), 2000);
                },
                ERROR_FILE_NOT_FOUND => {
                    eprintln!("relais : orchestrateur absent (aucun tube \\\\.\\pipe\\{tube}).");
                    return RELAIS_ABSENT;
                }
                e => {
                    eprintln!("relais : ouverture du tube \\\\.\\pipe\\{tube} impossible (erreur {e}).");
                    return RELAIS_PERDU;
                }
            }
        };
        let tube = Arc::new(Tube(h));
        super::pomper(Lecteur(tube.clone()), Ecrivain(tube))
    }
}

#[cfg(unix)]
mod imp {
    use std::io::{self, Read, Write};
    use std::os::unix::io::AsRawFd;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::PathBuf;

    use commun::protocole::{RELAIS_ABSENT, chemin_socket};

    /// Hors Windows, l'unicité vient de la socket : `ecouter` refuse si une instance y répond déjà.
    pub fn instance_unique(_tube: &str) -> Result<(), ()> {
        Ok(())
    }

    pub struct Ecoute {
        auditeur: UnixListener,
        chemin: PathBuf,
    }

    impl Drop for Ecoute {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.chemin);
        }
    }

    impl Ecoute {
        pub fn accepter(&mut self) -> io::Result<Flux> {
            self.auditeur.accept().map(|(flux, _)| Flux { flux })
        }
    }

    pub fn ecouter(tube: &str) -> io::Result<Ecoute> {
        let chemin = chemin_socket(tube);
        let auditeur = match UnixListener::bind(&chemin) {
            Ok(a) => a,
            Err(e) if e.kind() == io::ErrorKind::AddrInUse => {
                if UnixStream::connect(&chemin).is_ok() {
                    return Err(io::Error::new(io::ErrorKind::AddrInUse, "une instance répond déjà sur cette socket"));
                }
                std::fs::remove_file(&chemin)?;
                UnixListener::bind(&chemin)?
            }
            Err(e) => return Err(e),
        };
        Ok(Ecoute { auditeur, chemin })
    }

    pub struct Flux {
        flux: UnixStream,
    }

    impl Flux {
        pub fn parties(&self) -> io::Result<(Box<dyn Read + Send>, Box<dyn Write + Send>)> {
            Ok((Box::new(self.flux.try_clone()?), Box::new(self.flux.try_clone()?)))
        }

        pub fn vivant(&self) -> bool {
            let mut octet = 0u8;
            let n = unsafe {
                libc::recv(self.flux.as_raw_fd(), (&mut octet as *mut u8).cast(), 1, libc::MSG_PEEK | libc::MSG_DONTWAIT)
            };
            n > 0 || (n < 0 && io::Error::last_os_error().kind() == io::ErrorKind::WouldBlock)
        }
    }

    pub fn relais(tube: &str) -> i32 {
        match UnixStream::connect(chemin_socket(tube)) {
            Ok(flux) => match flux.try_clone() {
                Ok(lecture) => super::pomper(lecture, flux),
                Err(_) => RELAIS_ABSENT,
            },
            Err(_) => {
                eprintln!("relais : orchestrateur absent (aucune socket pour le tube {tube}).");
                RELAIS_ABSENT
            }
        }
    }
}
