//! Hardware detection → a coarse capability tier.
//!
//! Detected once (RAM + logical cores, no extra dependencies) and cached in a
//! `OnceLock`. Consumers scale their budgets from it so a weak laptop stays
//! responsive and a big workstation gets full quality:
//!   • `CommandHistory` sizes its undo memory budget from total RAM,
//!   • `main()` sizes the rayon worker pool (weak machines keep cores free
//!     for the UI thread),
//!   • the Develop fast-preview proxy targets fewer pixels on Low.

use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HwTier {
    Low,
    Mid,
    High,
}

#[derive(Debug, Clone, Copy)]
pub struct HwInfo {
    /// Total physical RAM in bytes; 0 when detection failed.
    pub total_ram_bytes: u64,
    pub logical_cores: usize,
    pub tier: HwTier,
}

static HW: OnceLock<HwInfo> = OnceLock::new();

#[derive(Debug, Clone)]
pub struct GpuInfo {
    pub name: String,
    pub device_type: String,
    pub backend: String,
    /// True for a real discrete/integrated/virtual GPU. Software adapters do
    /// not qualify, because DirectML would only add overhead there.
    pub ai_candidate: bool,
}

static GPU: OnceLock<GpuInfo> = OnceLock::new();

/// Called once by `GpuState` after wgpu has selected the actual display
/// adapter. Retouch then uses the same hardware decision without probing the
/// system a second time on its worker thread.
pub fn record_gpu_adapter(info: GpuInfo) {
    let _ = GPU.set(info);
}

pub fn gpu() -> Option<&'static GpuInfo> {
    GPU.get()
}

pub fn ai_gpu_candidate() -> bool {
    gpu().is_some_and(|info| info.ai_candidate)
}

/// The detected hardware profile (detected on first call, then cached).
pub fn get() -> &'static HwInfo {
    HW.get_or_init(detect)
}

fn detect() -> HwInfo {
    let logical_cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let total_ram_bytes = total_ram_bytes();
    HwInfo {
        total_ram_bytes,
        logical_cores,
        tier: tier_for(total_ram_bytes, logical_cores),
    }
}

/// Pure tier decision, separated for tests. Unknown RAM (0) falls back to a
/// core-count guess, leaning conservative.
fn tier_for(total_ram_bytes: u64, cores: usize) -> HwTier {
    const GIB: u64 = 1024 * 1024 * 1024;
    if total_ram_bytes == 0 {
        return if cores >= 8 { HwTier::Mid } else { HwTier::Low };
    }
    if total_ram_bytes >= 15 * GIB && cores >= 8 {
        HwTier::High
    } else if total_ram_bytes >= 7 * GIB && cores >= 4 {
        HwTier::Mid
    } else {
        HwTier::Low
    }
}

/// Undo-history memory budget: RAM/8, clamped to [256 MB, 3 GB]. History
/// entries are billed at MARGINAL cost (changed tiles only), so this buys tens
/// of full-layer edits on a 24MP photo instead of the old 4-5 — losing the way
/// back to the original image after a handful of steps is worse than spending
/// idle RAM on undo.
pub fn history_budget_bytes() -> usize {
    const MB: u64 = 1024 * 1024;
    let ram = get().total_ram_bytes;
    if ram == 0 {
        return (512 * MB) as usize;
    }
    (ram / 8).clamp(256 * MB, 3072 * MB) as usize
}

/// Worker threads for the global rayon pool: weak machines keep cores free so
/// the UI thread never starves under a bake; big machines use everything.
pub fn rayon_threads() -> usize {
    let hw = get();
    match hw.tier {
        HwTier::High => hw.logical_cores,
        HwTier::Mid => (hw.logical_cores.saturating_sub(1)).max(2),
        HwTier::Low => (hw.logical_cores.saturating_sub(2)).max(2),
    }
}

/// Pixel budget for the Develop fast-preview proxy (higher = sharper live
/// preview, more CPU per dragged frame).
pub fn fast_preview_target_pixels() -> usize {
    match get().tier {
        HwTier::High => 240_000,
        HwTier::Mid => 160_000,
        HwTier::Low => 90_000,
    }
}

#[cfg(windows)]
fn total_ram_bytes() -> u64 {
    #[repr(C)]
    struct MemoryStatusEx {
        dw_length: u32,
        dw_memory_load: u32,
        ull_total_phys: u64,
        ull_avail_phys: u64,
        ull_total_page_file: u64,
        ull_avail_page_file: u64,
        ull_total_virtual: u64,
        ull_avail_virtual: u64,
        ull_avail_extended_virtual: u64,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GlobalMemoryStatusEx(buffer: *mut MemoryStatusEx) -> i32;
    }
    let mut status = MemoryStatusEx {
        dw_length: std::mem::size_of::<MemoryStatusEx>() as u32,
        dw_memory_load: 0,
        ull_total_phys: 0,
        ull_avail_phys: 0,
        ull_total_page_file: 0,
        ull_avail_page_file: 0,
        ull_total_virtual: 0,
        ull_avail_virtual: 0,
        ull_avail_extended_virtual: 0,
    };
    // SAFETY: the struct matches the Win32 MEMORYSTATUSEX layout and dwLength
    // is set as the API requires.
    unsafe {
        if GlobalMemoryStatusEx(&mut status) != 0 {
            status.ull_total_phys
        } else {
            0
        }
    }
}

#[cfg(target_os = "linux")]
fn total_ram_bytes() -> u64 {
    // /proc/meminfo first line: "MemTotal:       16303428 kB"
    std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|s| {
            s.lines().find_map(|l| {
                l.strip_prefix("MemTotal:")?
                    .trim()
                    .strip_suffix("kB")
                    .and_then(|n| n.trim().parse::<u64>().ok())
                    .map(|kb| kb * 1024)
            })
        })
        .unwrap_or(0)
}

#[cfg(target_os = "macos")]
fn total_ram_bytes() -> u64 {
    extern "C" {
        fn sysctlbyname(
            name: *const std::ffi::c_char,
            oldp: *mut std::ffi::c_void,
            oldlenp: *mut usize,
            newp: *mut std::ffi::c_void,
            newlen: usize,
        ) -> i32;
    }
    let mut value: u64 = 0;
    let mut len = std::mem::size_of::<u64>();
    // SAFETY: hw.memsize is a u64 sysctl; buffer and length match.
    unsafe {
        let name = c"hw.memsize";
        if sysctlbyname(
            name.as_ptr(),
            &mut value as *mut u64 as *mut std::ffi::c_void,
            &mut len,
            std::ptr::null_mut(),
            0,
        ) == 0
        {
            value
        } else {
            0
        }
    }
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn total_ram_bytes() -> u64 {
    0
}

/// Windows throttles the CPU of a process it considers background (EcoQoS)
/// once it has run hot for a few seconds; a RAW decode then takes ~2.5× as
/// long. Opt the whole process out so heavy work runs at full speed.
pub fn opt_out_of_power_throttling() {
    #[cfg(windows)]
    {
        #[repr(C)]
        struct ProcessPowerThrottlingState {
            version: u32,
            control_mask: u32,
            state_mask: u32,
        }
        const PROCESS_POWER_THROTTLING: i32 = 4;
        const PROCESS_POWER_THROTTLING_CURRENT_VERSION: u32 = 1;
        const PROCESS_POWER_THROTTLING_EXECUTION_SPEED: u32 = 0x1;
        #[link(name = "kernel32")]
        extern "system" {
            fn GetCurrentProcess() -> isize;
            fn SetProcessInformation(
                process: isize,
                class: i32,
                information: *const std::ffi::c_void,
                size: u32,
            ) -> i32;
        }
        let state = ProcessPowerThrottlingState {
            version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
            control_mask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
            state_mask: 0,
        };
        // SAFETY: the struct matches PROCESS_POWER_THROTTLING_STATE; the call
        // only fails on systems that predate the class, which is harmless.
        unsafe {
            SetProcessInformation(
                GetCurrentProcess(),
                PROCESS_POWER_THROTTLING,
                &state as *const ProcessPowerThrottlingState as *const std::ffi::c_void,
                std::mem::size_of::<ProcessPowerThrottlingState>() as u32,
            );
        }
    }
}

/// Drop the calling thread below normal priority, for background work (RAW
/// prefetch) that must yield to the UI and to the image the user is waiting on.
pub fn lower_current_thread_priority() {
    #[cfg(windows)]
    {
        const THREAD_PRIORITY_BELOW_NORMAL: i32 = -1;
        #[link(name = "kernel32")]
        extern "system" {
            fn GetCurrentThread() -> isize;
            fn SetThreadPriority(thread: isize, priority: i32) -> i32;
        }
        // SAFETY: the pseudo-handle always refers to the calling thread.
        unsafe {
            SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
        }
    }
}

/// Free bytes available to this user on the volume holding `path`; `None`
/// when unknown.
pub fn free_disk_bytes(path: &std::path::Path) -> Option<u64> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        #[link(name = "kernel32")]
        extern "system" {
            fn GetDiskFreeSpaceExW(
                directory: *const u16,
                free_to_caller: *mut u64,
                total: *mut u64,
                total_free: *mut u64,
            ) -> i32;
        }
        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let mut free = 0u64;
        // SAFETY: `wide` is NUL-terminated; the optional outputs may be null.
        let ok = unsafe {
            GetDiskFreeSpaceExW(
                wide.as_ptr(),
                &mut free,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        (ok != 0).then_some(free)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detection_returns_sane_values() {
        let hw = get();
        assert!(hw.logical_cores >= 1);
        // RAM may legitimately be unknown (0) on exotic platforms, but when
        // known it should be at least 256 MB on anything that runs this app.
        if hw.total_ram_bytes > 0 {
            assert!(hw.total_ram_bytes >= 256 * 1024 * 1024);
        }
        assert!(rayon_threads() >= 2);
        assert!(history_budget_bytes() >= 128 * 1024 * 1024);
    }

    #[test]
    fn tier_decision_matches_spec() {
        const GIB: u64 = 1024 * 1024 * 1024;
        assert_eq!(tier_for(32 * GIB, 16), HwTier::High);
        assert_eq!(tier_for(16 * GIB, 8), HwTier::High);
        assert_eq!(tier_for(8 * GIB, 4), HwTier::Mid);
        assert_eq!(tier_for(16 * GIB, 4), HwTier::Mid, "few cores caps at Mid");
        assert_eq!(tier_for(4 * GIB, 8), HwTier::Low, "low RAM is Low");
        assert_eq!(tier_for(0, 16), HwTier::Mid, "unknown RAM: conservative");
        assert_eq!(tier_for(0, 4), HwTier::Low);
    }
}
