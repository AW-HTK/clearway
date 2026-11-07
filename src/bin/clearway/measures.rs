use std::path::Path;

use clearway::model::Measures;

#[cfg(windows)]
pub use windows::Sensor;

#[cfg(not(windows))]
pub use other::Sensor;

/// Offsets of the useful fields in an entry of the system process list (`SYSTEM_PROCESS_INFORMATION`).
#[derive(Clone, Copy)]
#[cfg_attr(not(windows), allow(dead_code))]
pub struct Layout {
    pub size: usize,
    pub next: usize,
    pub name_length: usize,
    pub name_buffer: usize,
    pub pid: usize,
    pub memory: usize,
}

/// Finds a process by name in the list returned by the kernel, never reading outside the filled `bytes`; `address` is
/// that of the first byte, because names are given there as absolute pointers. Returns the PID and resident memory in bytes.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn find_process(bytes: &[u8], address: usize, d: &Layout, name: &str) -> Option<(u32, u64)> {
    let read = |began: usize, n: usize| -> Option<u64> {
        let field = bytes.get(began..began.checked_add(n)?)?;
        let mut buffer = [0u8; 8];
        buffer[..n].copy_from_slice(field);
        Some(u64::from_le_bytes(buffer))
    };
    let pointer = size_of::<usize>();
    let mut input = 0usize;
    loop {
        if input.checked_add(d.size)? > bytes.len() {
            return None;
        }
        let length = read(input + d.name_length, 2)? as usize;
        let buffer = read(input + d.name_buffer, pointer)? as usize;
        let text = buffer
            .checked_sub(address)
            .filter(|_| length > 0 && length.is_multiple_of(2))
            .and_then(|began| bytes.get(began..began.checked_add(length)?));
        if let Some(text) = text {
            let units: Vec<u16> = text.as_chunks::<2>().0.iter().map(|&c| u16::from_le_bytes(c)).collect();
            if String::from_utf16_lossy(&units).eq_ignore_ascii_case(name) {
                return Some((read(input + d.pid, pointer)? as u32, read(input + d.memory, pointer)?));
            }
        }
        let next = read(input + d.next, 4)? as usize;
        if next == 0 {
            return None;
        }
        input = input.checked_add(next)?;
    }
}

#[cfg(windows)]
mod windows {
    use std::mem::offset_of;
    use std::path::{Path, PathBuf};

    use clearway::model::{Measures, Vmmem};
    use windows_sys::Wdk::System::SystemInformation::{NtQuerySystemInformation, SystemProcessInformation};
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::ProcessStatus::{GetPerformanceInfo, PERFORMANCE_INFORMATION};
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    use windows_sys::Win32::System::Threading::GetSystemTimes;
    use windows_sys::Win32::System::WindowsProgramming::SYSTEM_PROCESS_INFORMATION;

    const MIB: u64 = 1024 * 1024;
    const STATUS_INFO_LENGTH_MISMATCH: i32 = 0xC000_0004_u32 as i32;
    const BUFFER_MAX_BYTES: usize = 32 * 1024 * 1024;

    const LAYOUT: super::Layout = super::Layout {
        size: size_of::<SYSTEM_PROCESS_INFORMATION>(),
        next: offset_of!(SYSTEM_PROCESS_INFORMATION, NextEntryOffset),
        name_length: offset_of!(SYSTEM_PROCESS_INFORMATION, ImageName.Length),
        name_buffer: offset_of!(SYSTEM_PROCESS_INFORMATION, ImageName.Buffer),
        pid: offset_of!(SYSTEM_PROCESS_INFORMATION, UniqueProcessId),
        memory: offset_of!(SYSTEM_PROCESS_INFORMATION, WorkingSetSize),
    };

    pub struct Sensor {
        cpu: Option<(u64, u64)>,
        buffer: Vec<u64>,
        vmmem: Option<Vmmem>,
        memory: Option<(u64, u64)>,
        dir: PathBuf,
        reported: bool,
    }

    fn value(ft: &FILETIME) -> u64 {
        ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64
    }

    impl Sensor {
        pub fn new(dir: &Path) -> Sensor {
            Sensor {
                cpu: None,
                buffer: vec![0; 32 * 1024],
                vmmem: None,
                memory: None,
                dir: dir.to_path_buf(),
                reported: false,
            }
        }

        fn report(&mut self, text: &str) {
            if !self.reported {
                self.reported = true;
                crate::server::log_line(
                    &self.dir,
                    &format!(
                        "ERROR {}",
                        clearway::tr!("measures: {text}; last values kept.", "mesures : {text} ; dernières valeurs gardées.")
                    ),
                );
            }
        }

        /// vmmemWSL refuses OpenProcess even with limited read access: its resident memory can only be read from the
        /// system process list, the one Get-Process uses.
        fn read_vmmem(&mut self) -> Option<Vmmem> {
            let filled = loop {
                let capacity = self.buffer.len() * 8;
                let mut required = 0u32;
                let status_view = unsafe {
                    NtQuerySystemInformation(SystemProcessInformation, self.buffer.as_mut_ptr().cast(), capacity as u32, &mut required)
                };
                if status_view == STATUS_INFO_LENGTH_MISMATCH {
                    let wanted = (required as usize).saturating_add(64 * 1024).min(BUFFER_MAX_BYTES);
                    if wanted > capacity {
                        self.buffer.resize(wanted.div_ceil(8), 0);
                        continue;
                    }
                }
                if status_view < 0 {
                    return self.vmmem;
                }
                break if required == 0 { capacity } else { (required as usize).min(capacity) };
            };
            // Vec<u64>: alignment enough for the kernel; only the bytes it filled are read back.
            let bytes = unsafe { std::slice::from_raw_parts(self.buffer.as_ptr().cast::<u8>(), filled) };
            self.vmmem = super::find_process(bytes, bytes.as_ptr() as usize, &LAYOUT, "vmmemWSL")
                .map(|(pid, rss)| Vmmem { pid, rss_mb: rss / MIB });
            self.vmmem
        }

        fn read_cpu(&mut self) -> f32 {
            let mut idle: FILETIME = unsafe { std::mem::zeroed() };
            let mut kernel: FILETIME = unsafe { std::mem::zeroed() };
            let mut user: FILETIME = unsafe { std::mem::zeroed() };
            if unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) } == 0 {
                return 0.0;
            }
            // Kernel time includes idle time.
            let total = value(&kernel) + value(&user);
            let idle = value(&idle);
            let pct = match self.cpu {
                Some((t0, i0)) if total > t0 => 100.0 * (1.0 - (idle - i0) as f64 / (total - t0) as f64),
                _ => 0.0,
            };
            self.cpu = Some((total, idle));
            pct.clamp(0.0, 100.0) as f32
        }

        pub fn read(&mut self) -> Measures {
            let mut memory: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
            memory.dwLength = size_of::<MEMORYSTATUSEX>() as u32;
            if unsafe { GlobalMemoryStatusEx(&mut memory) } != 0 {
                self.memory = Some((memory.ullTotalPhys / MIB, memory.ullAvailPhys / MIB));
            } else {
                let e = std::io::Error::last_os_error();
                self.report(&clearway::tr!("GlobalMemoryStatusEx failed ({e})", "GlobalMemoryStatusEx a échoué ({e})"));
            }
            let (ram_total_mb, ram_avail_mb) = self.memory.unwrap_or((0, 0));
            let mut perf: PERFORMANCE_INFORMATION = unsafe { std::mem::zeroed() };
            perf.cb = size_of::<PERFORMANCE_INFORMATION>() as u32;
            let perf_ok = unsafe { GetPerformanceInfo(&mut perf, perf.cb) } != 0;
            let page = perf.PageSize as u64;
            Measures {
                ram_total_mb,
                ram_avail_mb,
                commit_mb: if perf_ok { perf.CommitTotal as u64 * page / MIB } else { 0 },
                commit_limit_mb: if perf_ok { perf.CommitLimit as u64 * page / MIB } else { 0 },
                cpu_pct: self.read_cpu(),
                windows_cores: std::thread::available_parallelism().map_or(0, |n| n.get() as u32),
                swap_out_ps: 0.0,
                vmmem: self.read_vmmem(),
                wsl: None,
            }
        }
    }
}

#[cfg(not(windows))]
mod other {
    use std::path::{Path, PathBuf};

    use clearway::model::{Measures, Vmmem};

    /// Outside Windows (tests): fixed measures, replaceable by a `measures-test.json` file in the state folder.
    pub struct Sensor {
        file: PathBuf,
    }

    impl Sensor {
        pub fn new(dir: &Path) -> Sensor {
            Sensor { file: dir.join("measures-test.json") }
        }

        pub fn read(&mut self) -> Measures {
            std::fs::read(&self.file)
                .ok()
                .and_then(|o| serde_json::from_slice(&o).ok())
                .unwrap_or(Measures {
                    ram_total_mb: 32 * 1024,
                    ram_avail_mb: 20 * 1024,
                    commit_mb: 0,
                    commit_limit_mb: 0,
                    cpu_pct: 0.0,
                    windows_cores: 16,
                    swap_out_ps: 0.0,
                    vmmem: Some(Vmmem { pid: 4242, rss_mb: 4096 }),
                    wsl: None,
                })
        }
    }
}

pub fn first_measures(dir: &Path) -> (Sensor, Measures) {
    let mut c = Sensor::new(dir);
    let m = c.read();
    (c, m)
}

#[cfg(test)]
mod tests {
    use super::*;

    const D: Layout = Layout { size: 48, next: 0, name_length: 8, name_buffer: 16, pid: 24, memory: 32 };

    /// Fake list: one entry per (name, pid, memory, offset to the next), names stored after the entries.
    fn list(entries: &[(&str, u64, u64, u32)]) -> Vec<u8> {
        let mut bytes = vec![0u8; 4096];
        let address = bytes.as_ptr() as usize;
        let mut name_at = 2048;
        let mut input = 0;
        for &(name, pid, memory, next) in entries {
            let utf16: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
            bytes[name_at..name_at + utf16.len()].copy_from_slice(&utf16);
            bytes[input + D.next..input + D.next + 4].copy_from_slice(&next.to_le_bytes());
            bytes[input + D.name_length..input + D.name_length + 2].copy_from_slice(&(utf16.len() as u16).to_le_bytes());
            bytes[input + D.name_buffer..input + D.name_buffer + 8].copy_from_slice(&((address + name_at) as u64).to_le_bytes());
            bytes[input + D.pid..input + D.pid + 8].copy_from_slice(&pid.to_le_bytes());
            bytes[input + D.memory..input + D.memory + 8].copy_from_slice(&memory.to_le_bytes());
            name_at += 128;
            input += next as usize;
        }
        bytes
    }

    fn find(bytes: &[u8], length: usize) -> Option<(u32, u64)> {
        find_process(&bytes[..length], bytes.as_ptr() as usize, &D, "vmmemWSL")
    }

    #[test]
    fn finds_vmmem_in_a_sane_list() {
        let o = list(&[("System", 4, 1, 64), ("explorer.exe", 900, 2, 64), ("VmmemWSL", 4242, 8 << 30, 0)]);
        assert_eq!(find(&o, o.len()), Some((4242, 8 << 30)));
        let o = list(&[("System", 4, 1, 64), ("explorer.exe", 900, 2, 0)]);
        assert_eq!(find(&o, o.len()), None);
    }

    #[test]
    fn inconsistent_list_without_out_of_bounds_read() {
        // Next entry beyond what the kernel filled.
        let o = list(&[("System", 4, 1, 4000), ("vmmemWSL", 4242, 1, 0)]);
        assert_eq!(find(&o, 1024), None);
        // Fill truncated in the middle of the first entry.
        let o = list(&[("vmmemWSL", 4242, 1, 0)]);
        assert_eq!(find(&o, D.size - 1), None);
        // Name outside the filled buffer: entry skipped, the next one is read.
        let o = list(&[("vmmemWSL", 1, 1, 64), ("vmmemWSL", 4242, 7, 0)]);
        assert_eq!(find(&o, 2048 + 8), None);
        let mut o = list(&[("other", 1, 1, 64), ("vmmemWSL", 4242, 7, 0)]);
        o[D.name_buffer..D.name_buffer + 8].copy_from_slice(&u64::MAX.to_le_bytes());
        assert_eq!(find(&o, o.len()), Some((4242, 7)));
        // Odd or oversized name length.
        let mut o = list(&[("vmmemWSL", 4242, 7, 0)]);
        o[D.name_length..D.name_length + 2].copy_from_slice(&15u16.to_le_bytes());
        assert_eq!(find(&o, o.len()), None);
        o[D.name_length..D.name_length + 2].copy_from_slice(&u16::MAX.to_le_bytes());
        assert_eq!(find(&o, o.len()), None);
        // Huge offset: no addition overflow.
        let o = list(&[("System", 4, 1, u32::MAX)]);
        assert_eq!(find(&o, o.len()), None);
    }
}
