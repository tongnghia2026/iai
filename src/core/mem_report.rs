//! Memory Milestone M0 — logical memory accounting + process working-set probe.
//!
//! The RAW-RAM plan (`docs/planning/KE_HOACH_GIAM_RAM_MO_NHIEU_RAW_2026-08-25.md`)
//! opens with a measurement gate: before changing any document ownership we must
//! be able to *explain* the ~12 GB resident set observed when the standard corpus
//! is fully opened. This module is that instrument.
//!
//! It is deliberately **non-invasive**: instead of hooking every allocation, the
//! byte-owning types expose walk-the-structure accessors (`resident_bytes` /
//! `account_memory`) that a [`MemReport`] accumulates by class and by owner
//! document. The sum is a *logical* figure — the bytes the app is holding on
//! purpose — which is then compared against the OS process working set
//! ([`process_working_set_bytes`]). If logical ≈ working set, the report explains
//! the footprint and there is no hidden leak; the plan's conclusion (§3.3) is that
//! the 12 GB is by-design retention, and this module lets a test prove it.
//!
//! No new crate dependency: the process-memory query mirrors the direct
//! `extern "system"` FFI already used by [`crate::core::hw`] for physical RAM.

use std::collections::BTreeMap;

/// One class of resident buffer. Every heap allocation the plan calls out (§1
/// table, §3.2) maps to exactly one variant, so a per-class total is directly
/// comparable to the plan's estimate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemClass {
    /// [`crate::core::develop_scene::SceneSource::half`] — RGBA f16 scene master
    /// (8 B/px). The linear scene-referred RAW master kept for the Develop stage.
    SceneHalf,
    /// [`crate::core::develop_scene::SceneSource::alpha`] — exact UNORM16 alpha
    /// for Identity scenes with transparency (2 B/px). `None` for RAW.
    SceneAlpha,
    /// Tile `pixels16` — the RGBA16 master of a 16-bit document (8 B/px).
    TileRgba16,
    /// Tile `pixels` — the RGBA8 display/tool mirror every tile carries (4 B/px).
    TileRgba8,
    /// Tile `ink` — CMYK8 ink planes for a CMYK-mode document (4 B/px).
    TileInk,
    /// A layer mask's tiles (a `LayerMask` is itself a small tile canvas).
    LayerMask,
    /// [`crate::core::canvas::Canvas::pixels`] — the flat RGBA8 composite kept for
    /// CPU tools/export on canvases up to `LARGE_CANVAS_PIXELS` (4 B/px).
    FlatCanvas,
    /// [`crate::core::selection::Selection::mask`] — one byte per canvas pixel.
    SelectionMask,
    /// Smart-Select `EdgeCache` — Lab (`[f32;3]`) + Sobel (`f32`) per pixel.
    EdgeCache,
    /// Saved alpha channels (Channels panel) — one `Vec<u8>` mask each.
    ChannelAlpha,
    /// Undo/redo history — billed at the marginal (changed-tile) cost the
    /// `CommandHistory` already tracks.
    History,
    /// GPU-resident bytes (textures/atlas) when measurable. Reserved; the CPU
    /// harness leaves this at zero because it never uploads to a GPU.
    Gpu,
}

impl MemClass {
    /// Every class, in report order.
    pub const ALL: [MemClass; 12] = [
        MemClass::SceneHalf,
        MemClass::SceneAlpha,
        MemClass::TileRgba16,
        MemClass::TileRgba8,
        MemClass::TileInk,
        MemClass::LayerMask,
        MemClass::FlatCanvas,
        MemClass::SelectionMask,
        MemClass::EdgeCache,
        MemClass::ChannelAlpha,
        MemClass::History,
        MemClass::Gpu,
    ];

    #[inline]
    pub fn index(self) -> usize {
        match self {
            MemClass::SceneHalf => 0,
            MemClass::SceneAlpha => 1,
            MemClass::TileRgba16 => 2,
            MemClass::TileRgba8 => 3,
            MemClass::TileInk => 4,
            MemClass::LayerMask => 5,
            MemClass::FlatCanvas => 6,
            MemClass::SelectionMask => 7,
            MemClass::EdgeCache => 8,
            MemClass::ChannelAlpha => 9,
            MemClass::History => 10,
            MemClass::Gpu => 11,
        }
    }

    /// Stable machine key (used in the JSON summary and as a table row label).
    pub fn key(self) -> &'static str {
        match self {
            MemClass::SceneHalf => "scene_half_f16",
            MemClass::SceneAlpha => "scene_alpha_u16",
            MemClass::TileRgba16 => "tile_rgba16",
            MemClass::TileRgba8 => "tile_rgba8",
            MemClass::TileInk => "tile_ink_cmyk",
            MemClass::LayerMask => "layer_mask",
            MemClass::FlatCanvas => "flat_canvas_rgba8",
            MemClass::SelectionMask => "selection_mask",
            MemClass::EdgeCache => "edge_cache",
            MemClass::ChannelAlpha => "channel_alpha",
            MemClass::History => "history",
            MemClass::Gpu => "gpu",
        }
    }
}

const N_CLASSES: usize = 12;

/// An accumulating logical-memory snapshot: bytes by class and by owner.
///
/// Owners are document-scoped labels (a file name in the corpus harness, a
/// document title in the app) so the plan's "owner document" diagnostic (§M0) is
/// available without threading a document id through every accessor.
#[derive(Debug, Clone, Default)]
pub struct MemReport {
    by_class: [u64; N_CLASSES],
    by_owner: BTreeMap<String, u64>,
}

impl MemReport {
    pub fn new() -> Self {
        Self::default()
    }

    /// Charge `bytes` of `class` to `owner`.
    #[inline]
    pub fn add(&mut self, class: MemClass, owner: &str, bytes: u64) {
        if bytes == 0 {
            return;
        }
        self.by_class[class.index()] += bytes;
        *self.by_owner.entry(owner.to_string()).or_insert(0) += bytes;
    }

    #[inline]
    pub fn class_bytes(&self, class: MemClass) -> u64 {
        self.by_class[class.index()]
    }

    #[inline]
    pub fn total(&self) -> u64 {
        self.by_class.iter().sum()
    }

    /// Number of distinct owners charged so far.
    pub fn owner_count(&self) -> usize {
        self.by_owner.len()
    }

    /// Fold another report into this one (per-class and per-owner).
    pub fn merge(&mut self, other: &MemReport) {
        for (i, b) in other.by_class.iter().enumerate() {
            self.by_class[i] += *b;
        }
        for (owner, b) in &other.by_owner {
            *self.by_owner.entry(owner.clone()).or_insert(0) += *b;
        }
    }

    /// Owners sorted by descending bytes (heaviest first) — the app-side
    /// diagnostic answers "which document is holding the RAM?".
    pub fn owners_by_bytes(&self) -> Vec<(String, u64)> {
        let mut v: Vec<(String, u64)> =
            self.by_owner.iter().map(|(k, v)| (k.clone(), *v)).collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        v
    }

    /// A human-readable per-class table (MiB), highest classes intact for a
    /// quick eyeball in a `--nocapture` test run.
    pub fn format_class_table(&self) -> String {
        let mut s = String::new();
        s.push_str(&format!("{:<20} {:>12} {:>10}\n", "class", "bytes", "MiB"));
        for class in MemClass::ALL {
            let b = self.class_bytes(class);
            if b == 0 {
                continue;
            }
            s.push_str(&format!("{:<20} {:>12} {:>10.2}\n", class.key(), b, mib(b)));
        }
        let t = self.total();
        s.push_str(&format!(
            "{:<20} {:>12} {:>10.2}\n",
            "TOTAL_LOGICAL",
            t,
            mib(t)
        ));
        s
    }

    /// Single-line machine-readable JSON of the per-class totals plus the grand
    /// total. Hand-rolled (no serde dependency) and stable-keyed so a harness or
    /// script can diff two baselines. `extra` key/value pairs (already-numeric)
    /// are appended verbatim — used to fold in the process working set.
    pub fn to_json(&self, extra: &[(&str, u64)]) -> String {
        let mut parts: Vec<String> = Vec::new();
        for class in MemClass::ALL {
            parts.push(format!("\"{}\":{}", class.key(), self.class_bytes(class)));
        }
        parts.push(format!("\"total_logical\":{}", self.total()));
        parts.push(format!("\"owners\":{}", self.owner_count()));
        for (k, v) in extra {
            parts.push(format!("\"{k}\":{v}"));
        }
        format!("{{{}}}", parts.join(","))
    }
}

/// Bytes → MiB as f64 (for display only).
#[inline]
pub fn mib(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

// ── Process memory (OS working set) ─────────────────────────────────────────
//
// A cross-check against the logical figure, never a substitute for it (the plan
// is explicit: "không dùng working set thay cho logical accounting"). Mirrors the
// direct-FFI style of `core::hw` so it adds no dependency.

/// Current + peak process working-set size in bytes, or `None` when the platform
/// query is unavailable. The peak is the high-water mark the OS has seen for this
/// process, which is exactly the per-stage peak the plan asks the harness to
/// report across decode/open/attach/switch/close.
pub fn process_working_set() -> Option<ProcessMemory> {
    platform::working_set()
}

/// Convenience: just the current working-set size.
pub fn process_working_set_bytes() -> Option<u64> {
    process_working_set().map(|m| m.working_set)
}

#[derive(Debug, Clone, Copy)]
pub struct ProcessMemory {
    /// Current resident/working set.
    pub working_set: u64,
    /// Peak working set the OS has recorded for this process.
    pub peak_working_set: u64,
}

#[cfg(windows)]
mod platform {
    use super::ProcessMemory;

    #[repr(C)]
    struct ProcessMemoryCounters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
    }

    // K32GetProcessMemoryInfo is exported by kernel32 (Win7+), so no extra
    // import library or crate feature is required.
    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> isize;
        fn K32GetProcessMemoryInfo(
            process: isize,
            counters: *mut ProcessMemoryCounters,
            cb: u32,
        ) -> i32;
    }

    pub fn working_set() -> Option<ProcessMemory> {
        let mut counters = ProcessMemoryCounters {
            cb: std::mem::size_of::<ProcessMemoryCounters>() as u32,
            page_fault_count: 0,
            peak_working_set_size: 0,
            working_set_size: 0,
            quota_peak_paged_pool_usage: 0,
            quota_paged_pool_usage: 0,
            quota_peak_non_paged_pool_usage: 0,
            quota_non_paged_pool_usage: 0,
            pagefile_usage: 0,
            peak_pagefile_usage: 0,
        };
        // SAFETY: the struct matches PROCESS_MEMORY_COUNTERS and `cb` is set to
        // its size as the API requires; the pseudo-handle from GetCurrentProcess
        // needs no close.
        unsafe {
            let ok = K32GetProcessMemoryInfo(
                GetCurrentProcess(),
                &mut counters,
                std::mem::size_of::<ProcessMemoryCounters>() as u32,
            );
            if ok != 0 {
                Some(ProcessMemory {
                    working_set: counters.working_set_size as u64,
                    peak_working_set: counters.peak_working_set_size as u64,
                })
            } else {
                None
            }
        }
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::ProcessMemory;

    /// `/proc/self/statm`: fields are page counts; the second is resident set.
    /// `/proc/self/status` VmHWM gives the peak; fall back to current if absent.
    pub fn working_set() -> Option<ProcessMemory> {
        let page = 4096u64; // getpagesize without libc; overwhelmingly 4 KiB on x86_64.
        let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
        let resident_pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
        let working_set = resident_pages * page;
        let peak = std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|s| {
                s.lines().find_map(|l| {
                    l.strip_prefix("VmHWM:")?
                        .trim()
                        .strip_suffix("kB")
                        .and_then(|n| n.trim().parse::<u64>().ok())
                        .map(|kb| kb * 1024)
                })
            })
            .unwrap_or(working_set);
        Some(ProcessMemory {
            working_set,
            peak_working_set: peak,
        })
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
mod platform {
    use super::ProcessMemory;
    pub fn working_set() -> Option<ProcessMemory> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_accumulates_by_class_and_owner() {
        let mut r = MemReport::new();
        r.add(MemClass::SceneHalf, "a.nef", 1000);
        r.add(MemClass::TileRgba8, "a.nef", 400);
        r.add(MemClass::SceneHalf, "b.cr2", 2000);
        assert_eq!(r.class_bytes(MemClass::SceneHalf), 3000);
        assert_eq!(r.class_bytes(MemClass::TileRgba8), 400);
        assert_eq!(r.total(), 3400);
        assert_eq!(r.owner_count(), 2);
        let owners = r.owners_by_bytes();
        assert_eq!(owners[0], ("b.cr2".to_string(), 2000));
        assert_eq!(owners[1], ("a.nef".to_string(), 1400));
    }

    #[test]
    fn add_zero_is_a_noop() {
        let mut r = MemReport::new();
        r.add(MemClass::History, "x", 0);
        assert_eq!(r.total(), 0);
        assert_eq!(r.owner_count(), 0);
    }

    #[test]
    fn merge_folds_both_axes() {
        let mut a = MemReport::new();
        a.add(MemClass::SceneHalf, "a", 10);
        let mut b = MemReport::new();
        b.add(MemClass::SceneHalf, "a", 5);
        b.add(MemClass::TileRgba16, "c", 7);
        a.merge(&b);
        assert_eq!(a.class_bytes(MemClass::SceneHalf), 15);
        assert_eq!(a.class_bytes(MemClass::TileRgba16), 7);
        assert_eq!(a.total(), 22);
        assert_eq!(a.owner_count(), 2);
    }

    #[test]
    fn json_has_every_class_and_extras() {
        let mut r = MemReport::new();
        r.add(MemClass::SceneHalf, "a", 8);
        let json = r.to_json(&[("working_set", 999)]);
        assert!(json.starts_with('{') && json.ends_with('}'));
        assert!(json.contains("\"scene_half_f16\":8"));
        assert!(json.contains("\"tile_rgba8\":0"));
        assert!(json.contains("\"total_logical\":8"));
        assert!(json.contains("\"working_set\":999"));
    }

    #[test]
    fn class_indices_are_unique_and_dense() {
        let mut seen = [false; N_CLASSES];
        for c in MemClass::ALL {
            let i = c.index();
            assert!(!seen[i], "duplicate index for {}", c.key());
            seen[i] = true;
        }
        assert!(seen.iter().all(|&s| s));
    }

    #[test]
    fn working_set_query_is_plausible_when_available() {
        // On the supported platforms this must return a non-zero figure; on
        // others `None` is acceptable. Either way it must not panic.
        if let Some(m) = process_working_set() {
            assert!(m.working_set > 0, "working set should be > 0");
            assert!(m.peak_working_set >= m.working_set);
        }
    }
}
