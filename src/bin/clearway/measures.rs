use std::collections::BTreeMap;
use std::path::Path;

use clearway::model::{Gpu, Measures};

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

/// Dedicated graphics card found at start: integrated and software adapters are left out.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(not(windows), allow(dead_code))]
pub struct Adapter {
    pub luid: String,
    pub name: String,
    pub vram_total_mb: u64,
}

/// Below this much dedicated video memory, an adapter is taken as integrated (they report 128 to 512 MB).
#[cfg_attr(not(windows), allow(dead_code))]
pub const DEDICATED_MIN_MB: u64 = 1024;

/// `luid_0x00000000_0x00010a60` from a GPU counter instance name, in lower case.
fn instance_luid(instance: &str) -> Option<String> {
    let lower = instance.to_lowercase();
    let start = lower.find("luid_")?;
    let rest = &lower[start..];
    let end = rest.find("_phys").unwrap_or(rest.len());
    Some(rest[..end].to_owned())
}

/// Per card: video memory used, and the busiest engine type (engines of one type summed over processes), as Task
/// Manager computes it.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn gpu_usage(adapters: &[Adapter], engines: Option<&[(String, f64)]>, memory: Option<&[(String, i64)]>) -> Vec<Gpu> {
    let mut busy: BTreeMap<(String, String), f64> = BTreeMap::new();
    for (instance, value) in engines.unwrap_or_default() {
        let lower = instance.to_lowercase();
        let (Some(luid), Some(kind)) = (instance_luid(&lower), lower.split("engtype_").nth(1)) else { continue };
        *busy.entry((luid, kind.to_owned())).or_default() += value;
    }
    adapters
        .iter()
        .map(|a| Gpu {
            luid: a.luid.clone(),
            name: a.name.clone(),
            vram_total_mb: a.vram_total_mb,
            vram_used_mb: memory.map(|m| {
                m.iter().filter(|(i, _)| instance_luid(i).as_deref() == Some(&a.luid)).map(|(_, b)| (*b).max(0) as u64).sum::<u64>() / MIB
            }),
            busy_pct: engines.map(|_| {
                busy.iter().filter(|((l, _), _)| *l == a.luid).map(|(_, v)| *v).fold(0.0, f64::max).min(100.0) as f32
            }),
        })
        .collect()
}

const MIB: u64 = 1024 * 1024;

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

    use super::MIB;
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
        gpu: super::gpu::GpuSensor,
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
                gpu: super::gpu::GpuSensor::new(),
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
                gpus: self.gpu.read(),
            }
        }
    }
}

#[cfg(windows)]
mod gpu {
    use windows_sys::Wdk::Graphics::Direct3D::{
        D3DKMT_ADAPTERINFO, D3DKMT_ADAPTERREGISTRYINFO, D3DKMT_ADAPTERTYPE, D3DKMT_CLOSEADAPTER, D3DKMT_ENUMADAPTERS2,
        D3DKMT_QUERYADAPTERINFO, D3DKMT_SEGMENTSIZEINFO, D3DKMTCloseAdapter, D3DKMTEnumAdapters2, D3DKMTQueryAdapterInfo,
        KMTQAITYPE_ADAPTERREGISTRYINFO, KMTQAITYPE_ADAPTERTYPE, KMTQAITYPE_GETSEGMENTSIZE, KMTQUERYADAPTERINFOTYPE,
    };
    use windows_sys::Win32::System::Performance::{
        PDH_CSTATUS_NEW_DATA, PDH_CSTATUS_VALID_DATA, PDH_FMT, PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_FMT_LARGE,
        PDH_HCOUNTER, PDH_HQUERY, PDH_MORE_DATA, PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData,
        PdhGetFormattedCounterArrayW, PdhOpenQueryW,
    };

    use super::{Adapter, DEDICATED_MIN_MB, MIB};
    use clearway::model::Gpu;
    use clearway::visual::wide;

    const PDH_FMT_NOCAP100: PDH_FMT = 0x8000;
    const RENDER_SUPPORTED: u32 = 1;
    const SOFTWARE_DEVICE: u32 = 1 << 2;

    /// Dedicated cards (D3DKMT, read once) and the Windows GPU counters, which Task Manager reads too.
    pub struct GpuSensor {
        adapters: Vec<Adapter>,
        query: PDH_HQUERY,
        engine: PDH_HCOUNTER,
        memory: PDH_HCOUNTER,
    }

    // The PDH handles are only used from the measuring thread that owns the sensor.
    unsafe impl Send for GpuSensor {}

    impl GpuSensor {
        pub fn new() -> GpuSensor {
            let adapters = dedicated_adapters();
            let (mut query, mut engine, mut memory) = (std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut());
            if !adapters.is_empty() && unsafe { PdhOpenQueryW(std::ptr::null(), 0, &mut query) } == 0 {
                let add = |path: &str, counter: &mut PDH_HCOUNTER| {
                    if unsafe { PdhAddEnglishCounterW(query, wide(path).as_ptr(), 0, counter) } != 0 {
                        *counter = std::ptr::null_mut();
                    }
                };
                add("\\GPU Engine(*)\\Utilization Percentage", &mut engine);
                add("\\GPU Adapter Memory(*)\\Dedicated Usage", &mut memory);
                unsafe { PdhCollectQueryData(query) };
            }
            GpuSensor { adapters, query, engine, memory }
        }

        pub fn read(&mut self) -> Vec<Gpu> {
            if self.adapters.is_empty() {
                return Vec::new();
            }
            let collected = !self.query.is_null() && unsafe { PdhCollectQueryData(self.query) } == 0;
            let engines = collected.then(|| values(self.engine, PDH_FMT_DOUBLE | PDH_FMT_NOCAP100, |v| unsafe { v.doubleValue })).flatten();
            let memory = collected.then(|| values(self.memory, PDH_FMT_LARGE, |v| unsafe { v.largeValue })).flatten();
            super::gpu_usage(&self.adapters, engines.as_deref(), memory.as_deref())
        }
    }

    impl Drop for GpuSensor {
        fn drop(&mut self) {
            if !self.query.is_null() {
                unsafe { PdhCloseQuery(self.query) };
            }
        }
    }

    /// Every instance of a wildcard counter with its value; `None` if the counter is missing or unreadable.
    fn values<T>(
        counter: PDH_HCOUNTER,
        format: PDH_FMT,
        read: impl Fn(&windows_sys::Win32::System::Performance::PDH_FMT_COUNTERVALUE_0) -> T,
    ) -> Option<Vec<(String, T)>> {
        if counter.is_null() {
            return None;
        }
        let (mut size, mut count) = (0u32, 0u32);
        let status = unsafe { PdhGetFormattedCounterArrayW(counter, format, &mut size, &mut count, std::ptr::null_mut()) };
        if status != PDH_MORE_DATA {
            return (status == 0).then(Vec::new);
        }
        // u64 storage: aligned for the items, whose names are stored after them in the same buffer.
        let mut buffer = vec![0u64; (size as usize).div_ceil(8)];
        let items = buffer.as_mut_ptr().cast::<PDH_FMT_COUNTERVALUE_ITEM_W>();
        if unsafe { PdhGetFormattedCounterArrayW(counter, format, &mut size, &mut count, items) } != 0 {
            return None;
        }
        let items = unsafe { std::slice::from_raw_parts(items, count as usize) };
        Some(
            items
                .iter()
                .filter(|i| matches!(i.FmtValue.CStatus, PDH_CSTATUS_VALID_DATA | PDH_CSTATUS_NEW_DATA))
                .map(|i| (unsafe { from_wide_ptr(i.szName) }, read(&i.FmtValue.Anonymous)))
                .collect(),
        )
    }

    unsafe fn from_wide_ptr(p: *const u16) -> String {
        if p.is_null() {
            return String::new();
        }
        let mut n = 0;
        while unsafe { *p.add(n) } != 0 {
            n += 1;
        }
        String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, n) })
    }

    fn query<T: Default>(handle: u32, kind: KMTQUERYADAPTERINFOTYPE) -> Option<T> {
        let mut value = T::default();
        let mut q = D3DKMT_QUERYADAPTERINFO {
            hAdapter: handle,
            Type: kind,
            pPrivateDriverData: (&mut value as *mut T).cast(),
            PrivateDriverDataSize: size_of::<T>() as u32,
        };
        (unsafe { D3DKMTQueryAdapterInfo(&mut q) } >= 0).then_some(value)
    }

    fn dedicated_adapters() -> Vec<Adapter> {
        let mut e = D3DKMT_ENUMADAPTERS2::default();
        if unsafe { D3DKMTEnumAdapters2(&mut e) } < 0 || e.NumAdapters == 0 {
            return Vec::new();
        }
        let mut infos = vec![D3DKMT_ADAPTERINFO::default(); e.NumAdapters as usize];
        e.pAdapters = infos.as_mut_ptr();
        if unsafe { D3DKMTEnumAdapters2(&mut e) } < 0 {
            return Vec::new();
        }
        let mut found: Vec<Adapter> = Vec::new();
        for info in &infos[..(e.NumAdapters as usize).min(infos.len())] {
            let kind = query::<D3DKMT_ADAPTERTYPE>(info.hAdapter, KMTQAITYPE_ADAPTERTYPE).map(|t| unsafe { t.Anonymous.Value });
            let size = query::<D3DKMT_SEGMENTSIZEINFO>(info.hAdapter, KMTQAITYPE_GETSEGMENTSIZE);
            let name = query::<D3DKMT_ADAPTERREGISTRYINFO>(info.hAdapter, KMTQAITYPE_ADAPTERREGISTRYINFO)
                .map(|r| String::from_utf16_lossy(&r.AdapterString).trim_end_matches('\0').trim().to_owned());
            unsafe { D3DKMTCloseAdapter(&D3DKMT_CLOSEADAPTER { hAdapter: info.hAdapter }) };
            let luid = format!("luid_0x{:08x}_0x{:08x}", info.AdapterLuid.HighPart as u32, info.AdapterLuid.LowPart);
            let vram_total_mb = size.map_or(0, |s| s.DedicatedVideoMemorySize / MIB);
            let render = kind.is_some_and(|k| k & RENDER_SUPPORTED != 0 && k & SOFTWARE_DEVICE == 0);
            if render && vram_total_mb >= DEDICATED_MIN_MB && !found.iter().any(|a| a.luid == luid) {
                found.push(Adapter { luid, name: name.filter(|n| !n.is_empty()).unwrap_or_else(|| "GPU".into()), vram_total_mb });
            }
        }
        found
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
                    gpus: Vec::new(),
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
    fn gpu_usage_per_card_from_counter_instances() {
        let rtx = Adapter { luid: "luid_0x00000000_0x00010a60".into(), name: "RTX".into(), vram_total_mb: 12288 };
        let engines = [
            ("pid_1476_luid_0x00000000_0x00010A60_phys_0_eng_0_engtype_3D".to_owned(), 30.0),
            ("pid_8_luid_0x00000000_0x00010a60_phys_0_eng_0_engtype_3D".to_owned(), 25.0),
            ("pid_9_luid_0x00000000_0x00010a60_phys_0_eng_1_engtype_Compute_0".to_owned(), 40.0),
            ("pid_9_luid_0x00000000_0x00011f77_phys_0_eng_0_engtype_3D".to_owned(), 90.0),
        ];
        let memory = [("luid_0x00000000_0x00010a60_phys_0".to_owned(), 2_000_000_000i64), ("luid_0x00000000_0x00011f77_phys_0".to_owned(), 5)];
        let g = &gpu_usage(std::slice::from_ref(&rtx), Some(&engines), Some(&memory))[0];
        assert_eq!((g.vram_used_mb, g.busy_pct), (Some(1907), Some(55.0)));
        let g = &gpu_usage(&[rtx], None, None)[0];
        assert_eq!((g.vram_total_mb, g.vram_used_mb, g.busy_pct), (12288, None, None));
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
