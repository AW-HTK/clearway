use std::io::{self, BufRead, BufReader, Read, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use clearway::protocol::{RELAY_LOST, read_line_limited};

use crate::server::{self, Shared};

#[cfg(windows)]
pub use imp::connect;
pub use imp::{Listener, legacy_instance, listen, relay, single_instance};

const MAX_CONNECTIONS: usize = 64;

/// Requests without `id` sent and responses without `id` received: at the end of stdin, the relay still returns those.
#[derive(Default)]
struct Sequential {
    sent: u64,
    received: u64,
    pipe_closed: bool,
}

fn without_id(line: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(line).map_or(true, |v| v.get("id").is_none_or(|i| i.is_null()))
}

/// Relays both ways at once: stdin lines to the pipe, pipe lines to stdout as soon as they arrive, which allows several
/// `id` requests in flight. At the end of stdin, the relay waits for the responses to requests without `id`
/// (`echo … | clearway relay`), but not for `id` requests: a multiplexed client that leaves must close its link at once.
fn pump(pipe_reader: impl Read + Send + 'static, mut pipe_writer: impl Write) -> i32 {
    let state = Arc::new((Mutex::new(Sequential::default()), Condvar::new()));
    {
        let state = state.clone();
        std::thread::spawn(move || {
            let mut back = BufReader::new(pipe_reader);
            let mut out = io::stdout();
            while let Ok(Some(r)) = read_line_limited(&mut back) {
                if writeln!(out, "{r}").and_then(|_| out.flush()).is_err() {
                    std::process::exit(0);
                }
                if without_id(&r) {
                    state.0.lock().unwrap_or_else(|e| e.into_inner()).received += 1;
                    state.1.notify_all();
                }
            }
            state.0.lock().unwrap_or_else(|e| e.into_inner()).pipe_closed = true;
            state.1.notify_all();
            eprintln!("{}", clearway::tr!("relay: Clearway closed the pipe.", "relais : Clearway a fermé le tube."));
            std::process::exit(RELAY_LOST);
        });
    }
    let mut input = io::stdin().lock();
    loop {
        let mut line = String::new();
        match input.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        if !line.ends_with('\n') {
            line.push('\n');
        }
        if !line.trim().is_empty() && without_id(&line) {
            state.0.lock().unwrap_or_else(|e| e.into_inner()).sent += 1;
        }
        if pipe_writer.write_all(line.as_bytes()).and_then(|_| pipe_writer.flush()).is_err() {
            eprintln!("{}", clearway::tr!("relay: Clearway closed the pipe.", "relais : Clearway a fermé le tube."));
            return RELAY_LOST;
        }
    }
    let (lock, signal) = &*state;
    let mut e = lock.lock().unwrap_or_else(|e| e.into_inner());
    while e.received < e.sent && !e.pipe_closed {
        e = signal.wait(e).unwrap_or_else(|e| e.into_inner());
    }
    0
}

struct Counter(Arc<AtomicUsize>);

impl Drop for Counter {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

pub fn serve(mut listening: Listener, p: Arc<Shared>) {
    let active = Arc::new(AtomicUsize::new(0));
    loop {
        let stream = match listening.accept() {
            Ok(f) => f,
            Err(_) => {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
        };
        if active.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
            active.fetch_sub(1, Ordering::SeqCst);
            continue;
        }
        let guard = Counter(active.clone());
        let p = p.clone();
        let stream = Arc::new(stream);
        let launched = std::thread::Builder::new().stack_size(256 * 1024).spawn(move || {
            let _guard = guard;
            if let Ok((read_half, writer)) = stream.halves() {
                let probe = stream.clone();
                server::serve_connection(&p, read_half, writer, server::Context::new(move || probe.alive()));
            }
        });
        if launched.is_err() {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::io::{self, Read, Write};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use clearway::protocol::{RELAY_ABSENT, RELAY_LOST};
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

    /// Pipe opened for overlapped I/O: on a synchronous handle, a pending read would block the other threads' writes.
    pub struct Pipe(HANDLE);

    // Each operation has its own OVERLAPPED structure and its own event.
    unsafe impl Send for Pipe {}
    unsafe impl Sync for Pipe {}

    impl Drop for Pipe {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
    }

    impl Pipe {
        fn operation(&self, launch: impl FnOnce(*mut OVERLAPPED) -> i32) -> io::Result<usize> {
            unsafe {
                let event = CreateEventW(std::ptr::null(), 1, 0, std::ptr::null());
                if event.is_null() {
                    return Err(io::Error::last_os_error());
                }
                let mut ov: OVERLAPPED = std::mem::zeroed();
                ov.hEvent = event;
                let mut n = 0u32;
                let result = if launch(&mut ov) != 0 || GetLastError() == ERROR_IO_PENDING {
                    if GetOverlappedResult(self.0, &ov, &mut n, 1) != 0 { Ok(n as usize) } else { Err(io::Error::last_os_error()) }
                } else {
                    Err(io::Error::last_os_error())
                };
                CloseHandle(event);
                result
            }
        }

        fn read(&self, buffer: &mut [u8]) -> io::Result<usize> {
            let length = buffer.len().min(u32::MAX as usize) as u32;
            let h = self.0;
            match self.operation(|ov| unsafe { ReadFile(h, buffer.as_mut_ptr(), length, std::ptr::null_mut(), ov) }) {
                Err(e) if [ERROR_BROKEN_PIPE, ERROR_PIPE_NOT_CONNECTED].map(|c| c as i32).contains(&e.raw_os_error().unwrap_or(0)) => Ok(0),
                r => r,
            }
        }

        fn write(&self, buffer: &[u8]) -> io::Result<usize> {
            let length = buffer.len().min(u32::MAX as usize) as u32;
            let h = self.0;
            self.operation(|ov| unsafe { WriteFile(h, buffer.as_ptr(), length, std::ptr::null_mut(), ov) })
        }
    }

    pub struct Reader(Arc<Pipe>);
    pub struct Writer(Arc<Pipe>);

    impl Read for Reader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            self.0.read(buffer)
        }
    }

    impl Write for Writer {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.0.write(buffer)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain([0]).collect()
    }

    fn name(pipe: &str) -> Vec<u16> {
        wide(&format!(r"\\.\pipe\{pipe}"))
    }

    fn user_sid() -> io::Result<String> {
        unsafe {
            let mut token: HANDLE = std::ptr::null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return Err(io::Error::last_os_error());
            }
            let mut buffer = vec![0u64; 64];
            let mut size = 0u32;
            let ok = GetTokenInformation(token, TokenUser, buffer.as_mut_ptr().cast(), (buffer.len() * 8) as u32, &mut size);
            let error = io::Error::last_os_error();
            CloseHandle(token);
            if ok == 0 {
                return Err(error);
            }
            let user = &*(buffer.as_ptr() as *const TOKEN_USER);
            let mut text: *mut u16 = std::ptr::null_mut();
            if ConvertSidToStringSidW(user.User.Sid, &mut text) == 0 {
                return Err(io::Error::last_os_error());
            }
            let length = (0..).take_while(|&i| *text.add(i) != 0).count();
            let sid = String::from_utf16_lossy(std::slice::from_raw_parts(text, length));
            LocalFree(text.cast());
            Ok(sid)
        }
    }

    /// Named mutex per pipe: one instance per pipe, but a test instance can run alongside.
    pub fn single_instance(pipe: &str) -> Result<HANDLE, ()> {
        mutex(&format!("Local\\clearway-{pipe}"))
    }

    /// Mutex of the pre-Clearway server: held while serving its pipe, so that an old copy launched by an old
    /// client stops at once.
    pub fn legacy_instance() -> Result<HANDLE, ()> {
        mutex("Local\\orchestrateur-charge-orchestrateur")
    }

    fn mutex(name: &str) -> Result<HANDLE, ()> {
        let name = wide(name);
        unsafe {
            let h = CreateMutexW(std::ptr::null(), 1, name.as_ptr());
            if h.is_null() || GetLastError() == ERROR_ALREADY_EXISTS {
                if !h.is_null() {
                    CloseHandle(h);
                }
                return Err(());
            }
            Ok(h)
        }
    }

    pub struct Listener {
        name: Vec<u16>,
        security: PSECURITY_DESCRIPTOR,
        next: Option<HANDLE>,
    }

    // The descriptor and the handles are only used by the listening thread.
    unsafe impl Send for Listener {}

    impl Drop for Listener {
        fn drop(&mut self) {
            unsafe {
                if let Some(h) = self.next.take() {
                    CloseHandle(h);
                }
                if !self.security.is_null() {
                    LocalFree(self.security.cast());
                }
            }
        }
    }

    impl Listener {
        fn create(&self, first: bool) -> io::Result<HANDLE> {
            let attributes = SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: self.security,
                bInheritHandle: 0,
            };
            let h = unsafe {
                CreateNamedPipeW(
                    self.name.as_ptr(),
                    PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | if first { FILE_FLAG_FIRST_PIPE_INSTANCE } else { 0 },
                    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                    PIPE_UNLIMITED_INSTANCES,
                    64 * 1024,
                    64 * 1024,
                    0,
                    &attributes,
                )
            };
            if h == INVALID_HANDLE_VALUE {
                let e = unsafe { GetLastError() };
                return Err(if e == ERROR_ACCESS_DENIED {
                    io::Error::new(
                        io::ErrorKind::AddrInUse,
                        clearway::tr!("another program already created this pipe", "un autre programme a déjà créé ce tube"),
                    )
                } else {
                    io::Error::from_raw_os_error(e as i32)
                });
            }
            Ok(h)
        }

        pub fn accept(&mut self) -> io::Result<Stream> {
            let h = match self.next.take() {
                Some(h) => h,
                None => self.create(false)?,
            };
            let pipe = Pipe(h);
            // Client already connected between creation and wait: the event will never be signalled.
            let connected = unsafe {
                let event = CreateEventW(std::ptr::null(), 1, 0, std::ptr::null());
                if event.is_null() {
                    return Err(io::Error::last_os_error());
                }
                let mut ov: OVERLAPPED = std::mem::zeroed();
                ov.hEvent = event;
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
                CloseHandle(event);
                r
            };
            connected.map(|()| Stream { pipe: Arc::new(pipe) })
        }
    }

    /// Pipe restricted to the current account (and SYSTEM), refused to remote clients.
    pub fn listen(pipe: &str) -> io::Result<Listener> {
        let sddl = wide(&format!("D:P(A;;GA;;;SY)(A;;GA;;;{})", user_sid()?));
        let mut security: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), SDDL_REVISION_1, &mut security, std::ptr::null_mut())
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let mut e = Listener { name: name(pipe), security, next: None };
        e.next = Some(e.create(true)?);
        Ok(e)
    }

    pub struct Stream {
        pipe: Arc<Pipe>,
    }

    impl Stream {
        pub fn halves(&self) -> io::Result<(Box<dyn Read + Send>, Box<dyn Write + Send>)> {
            Ok((Box::new(Reader(self.pipe.clone())), Box::new(Writer(self.pipe.clone()))))
        }

        pub fn alive(&self) -> bool {
            let mut available = 0u32;
            unsafe {
                PeekNamedPipe(
                    self.pipe.0,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null_mut(),
                    &mut available,
                    std::ptr::null_mut(),
                ) != 0
            }
        }
    }

    /// Client side of the pipe; `NotFound` when Clearway is not running.
    pub fn connect(pipe: &str) -> io::Result<(Box<dyn Read + Send>, Box<dyn Write + Send>)> {
        let name = name(pipe);
        let began = Instant::now();
        let h = loop {
            let h = unsafe {
                CreateFileW(
                    name.as_ptr(),
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
                ERROR_PIPE_BUSY if began.elapsed() < Duration::from_secs(10) => unsafe {
                    WaitNamedPipeW(name.as_ptr(), 2000);
                },
                ERROR_FILE_NOT_FOUND => return Err(io::Error::from(io::ErrorKind::NotFound)),
                e => return Err(io::Error::from_raw_os_error(e as i32)),
            }
        };
        let pipe = Arc::new(Pipe(h));
        Ok((Box::new(Reader(pipe.clone())), Box::new(Writer(pipe))))
    }

    pub fn relay(pipe: &str) -> i32 {
        match connect(pipe) {
            Ok((reader, writer)) => super::pump(reader, writer),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                eprintln!(
                    "{}",
                    clearway::tr!(
                        "relay: Clearway is not running (no pipe \\\\.\\pipe\\{pipe}).",
                        "relais : Clearway absent (aucun tube \\\\.\\pipe\\{pipe})."
                    )
                );
                RELAY_ABSENT
            }
            Err(e) => {
                eprintln!(
                    "{}",
                    clearway::tr!(
                        "relay: cannot open the pipe \\\\.\\pipe\\{pipe} ({e}).",
                        "relais : ouverture du tube \\\\.\\pipe\\{pipe} impossible ({e})."
                    )
                );
                RELAY_LOST
            }
        }
    }
}

#[cfg(unix)]
mod imp {
    use std::io::{self, Read, Write};
    use std::os::unix::io::AsRawFd;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::PathBuf;

    use clearway::protocol::{RELAY_ABSENT, socket_path};

    /// Outside Windows, uniqueness comes from the socket: `listen` refuses if an instance already answers on it.
    pub fn single_instance(_pipe: &str) -> Result<(), ()> {
        Ok(())
    }

    pub fn legacy_instance() -> Result<(), ()> {
        Ok(())
    }

    pub struct Listener {
        listener: UnixListener,
        path: PathBuf,
    }

    impl Drop for Listener {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    impl Listener {
        pub fn accept(&mut self) -> io::Result<Stream> {
            self.listener.accept().map(|(stream, _)| Stream { stream })
        }
    }

    pub fn listen(pipe: &str) -> io::Result<Listener> {
        let path = socket_path(pipe);
        let listener = match UnixListener::bind(&path) {
            Ok(a) => a,
            Err(e) if e.kind() == io::ErrorKind::AddrInUse => {
                if UnixStream::connect(&path).is_ok() {
                    return Err(io::Error::new(
                        io::ErrorKind::AddrInUse,
                        clearway::tr!("an instance already answers on this socket", "une instance répond déjà sur cette socket"),
                    ));
                }
                std::fs::remove_file(&path)?;
                UnixListener::bind(&path)?
            }
            Err(e) => return Err(e),
        };
        Ok(Listener { listener, path })
    }

    pub struct Stream {
        stream: UnixStream,
    }

    impl Stream {
        pub fn halves(&self) -> io::Result<(Box<dyn Read + Send>, Box<dyn Write + Send>)> {
            Ok((Box::new(self.stream.try_clone()?), Box::new(self.stream.try_clone()?)))
        }

        pub fn alive(&self) -> bool {
            let mut byte = 0u8;
            let n = unsafe {
                libc::recv(self.stream.as_raw_fd(), (&mut byte as *mut u8).cast(), 1, libc::MSG_PEEK | libc::MSG_DONTWAIT)
            };
            n > 0 || (n < 0 && io::Error::last_os_error().kind() == io::ErrorKind::WouldBlock)
        }
    }

    /// Client side of the socket; `NotFound` when Clearway is not running.
    pub fn connect(pipe: &str) -> io::Result<(Box<dyn Read + Send>, Box<dyn Write + Send>)> {
        let stream = UnixStream::connect(socket_path(pipe)).map_err(|_| io::Error::from(io::ErrorKind::NotFound))?;
        Ok((Box::new(stream.try_clone()?), Box::new(stream)))
    }

    pub fn relay(pipe: &str) -> i32 {
        match connect(pipe) {
            Ok((reader, writer)) => super::pump(reader, writer),
            Err(_) => {
                eprintln!(
                    "{}",
                    clearway::tr!(
                        "relay: Clearway is not running (no socket for pipe {pipe}).",
                        "relais : Clearway absent (aucune socket pour le tube {pipe})."
                    )
                );
                RELAY_ABSENT
            }
        }
    }
}
