use std::io::{self, BufRead, BufReader, Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use commun::protocole::{RELAIS_PERDU, lire_ligne};

use crate::serveur::{self, Partage};

pub use imp::{Ecoute, ecouter, instance_unique, relais};

const MAX_CONNEXIONS: usize = 64;

/// Recopie une requête de stdin vers le tube, puis sa réponse vers stdout, jusqu'à la fin de stdin.
fn pomper(tube_lecture: impl Read, mut tube_ecriture: impl Write) -> i32 {
    let mut entree = io::stdin().lock();
    let mut sortie = io::stdout().lock();
    let mut retour = BufReader::new(tube_lecture);
    loop {
        let mut ligne = String::new();
        match entree.read_line(&mut ligne) {
            Ok(0) | Err(_) => return 0,
            Ok(_) => {}
        }
        if !ligne.ends_with('\n') {
            ligne.push('\n');
        }
        if tube_ecriture.write_all(ligne.as_bytes()).and_then(|_| tube_ecriture.flush()).is_err() {
            eprintln!("relais : l'orchestrateur a fermé le tube.");
            return RELAIS_PERDU;
        }
        match lire_ligne(&mut retour) {
            Ok(Some(r)) => {
                if writeln!(sortie, "{r}").and_then(|_| sortie.flush()).is_err() {
                    return 0;
                }
            }
            _ => {
                eprintln!("relais : l'orchestrateur a fermé le tube avant de répondre.");
                return RELAIS_PERDU;
            }
        }
    }
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
        let lance = std::thread::Builder::new().stack_size(256 * 1024).spawn(move || {
            let _garde = garde;
            if let Ok((lecture, ecriture)) = flux.parties() {
                serveur::servir_connexion(&p, lecture, ecriture, &|| flux.vivant());
            }
        });
        if lance.is_err() {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::fs::File;
    use std::io::{self, Read, Write};
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use std::time::{Duration, Instant};

    use commun::protocole::{RELAIS_ABSENT, RELAIS_PERDU};
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, ERROR_PIPE_BUSY,
        ERROR_PIPE_CONNECTED, GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE, INVALID_HANDLE_VALUE, LocalFree,
    };
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
            CloseHandle(jeton);
            if ok == 0 {
                return Err(io::Error::last_os_error());
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
                    PIPE_ACCESS_DUPLEX | if premiere { FILE_FLAG_FIRST_PIPE_INSTANCE } else { 0 },
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
            let connecte = unsafe { ConnectNamedPipe(h, std::ptr::null_mut()) } != 0
                || unsafe { GetLastError() } == ERROR_PIPE_CONNECTED;
            if !connecte {
                let e = io::Error::last_os_error();
                unsafe { CloseHandle(h) };
                return Err(e);
            }
            Ok(Flux { fichier: unsafe { File::from_raw_handle(h) } })
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
        fichier: File,
    }

    impl Flux {
        pub fn parties(&self) -> io::Result<(Box<dyn Read + '_>, Box<dyn Write + '_>)> {
            Ok((Box::new(&self.fichier), Box::new(&self.fichier)))
        }

        pub fn vivant(&self) -> bool {
            let mut disponible = 0u32;
            unsafe {
                PeekNamedPipe(
                    self.fichier.as_raw_handle(),
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
                    0,
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
        let fichier = unsafe { File::from_raw_handle(h) };
        super::pomper(&fichier, &fichier)
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
        pub fn parties(&self) -> io::Result<(Box<dyn Read + '_>, Box<dyn Write + '_>)> {
            Ok((Box::new(self.flux.try_clone()?), Box::new(&self.flux)))
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
