//! Debug-only counters for repeatable hybrid-canvas measurements.

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub requested: u64,
    pub superseded: u64,
    pub completed: u64,
    pub objects: u64,
    pub supersample_pixels: u64,
    pub bake_micros: u64,
    // Phase 3 GPU mesh cache (observability for the "0 tessellation on
    // pan/zoom/move" budget). Counters accumulate; the two gauges are the last
    // observed cache size.
    /// Lyon tessellations run since start (a cache miss).
    pub mesh_tessellations: u64,
    /// GPU vertex/index buffer uploads since start (== tessellations).
    pub mesh_uploads: u64,
    /// Meshes evicted from the GPU cache since start (byte-budget LRU).
    pub mesh_evictions: u64,
    /// Current GPU mesh cache size in bytes (source vertex/index bytes).
    pub mesh_cache_bytes: u64,
    /// Current GPU mesh cache entry count.
    pub mesh_cache_entries: u64,
}

#[cfg(debug_assertions)]
mod imp {
    use super::Snapshot;
    use std::sync::atomic::{AtomicU64, Ordering};

    static REQUESTED: AtomicU64 = AtomicU64::new(0);
    static SUPERSEDED: AtomicU64 = AtomicU64::new(0);
    static COMPLETED: AtomicU64 = AtomicU64::new(0);
    static OBJECTS: AtomicU64 = AtomicU64::new(0);
    static PIXELS: AtomicU64 = AtomicU64::new(0);
    static MICROS: AtomicU64 = AtomicU64::new(0);
    static MESH_TESS: AtomicU64 = AtomicU64::new(0);
    static MESH_UPLOADS: AtomicU64 = AtomicU64::new(0);
    static MESH_EVICT: AtomicU64 = AtomicU64::new(0);
    static MESH_BYTES: AtomicU64 = AtomicU64::new(0);
    static MESH_ENTRIES: AtomicU64 = AtomicU64::new(0);

    pub fn request(superseded: bool) {
        REQUESTED.fetch_add(1, Ordering::Relaxed);
        if superseded {
            SUPERSEDED.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn complete(objects: usize, pixels: u64, micros: u64) {
        COMPLETED.fetch_add(1, Ordering::Relaxed);
        OBJECTS.fetch_add(objects as u64, Ordering::Relaxed);
        PIXELS.fetch_add(pixels, Ordering::Relaxed);
        MICROS.fetch_add(micros, Ordering::Relaxed);
    }

    /// Record one frame's GPU mesh-cache activity plus the resulting cache size.
    pub fn mesh_frame(
        tessellations: u32,
        uploads: u32,
        evictions: u64,
        bytes: usize,
        entries: usize,
    ) {
        if tessellations > 0 {
            MESH_TESS.fetch_add(tessellations as u64, Ordering::Relaxed);
        }
        if uploads > 0 {
            MESH_UPLOADS.fetch_add(uploads as u64, Ordering::Relaxed);
        }
        MESH_EVICT.store(evictions, Ordering::Relaxed);
        MESH_BYTES.store(bytes as u64, Ordering::Relaxed);
        MESH_ENTRIES.store(entries as u64, Ordering::Relaxed);
    }

    pub fn snapshot() -> Snapshot {
        Snapshot {
            requested: REQUESTED.load(Ordering::Relaxed),
            superseded: SUPERSEDED.load(Ordering::Relaxed),
            completed: COMPLETED.load(Ordering::Relaxed),
            objects: OBJECTS.load(Ordering::Relaxed),
            supersample_pixels: PIXELS.load(Ordering::Relaxed),
            bake_micros: MICROS.load(Ordering::Relaxed),
            mesh_tessellations: MESH_TESS.load(Ordering::Relaxed),
            mesh_uploads: MESH_UPLOADS.load(Ordering::Relaxed),
            mesh_evictions: MESH_EVICT.load(Ordering::Relaxed),
            mesh_cache_bytes: MESH_BYTES.load(Ordering::Relaxed),
            mesh_cache_entries: MESH_ENTRIES.load(Ordering::Relaxed),
        }
    }
}

#[cfg(debug_assertions)]
pub use imp::*;

#[cfg(not(debug_assertions))]
#[inline(always)]
pub fn request(_: bool) {}

#[cfg(not(debug_assertions))]
#[inline(always)]
pub fn complete(_: usize, _: u64, _: u64) {}

#[cfg(not(debug_assertions))]
#[inline(always)]
pub fn mesh_frame(_: u32, _: u32, _: u64, _: usize, _: usize) {}

#[cfg(not(debug_assertions))]
#[inline(always)]
pub fn snapshot() -> Snapshot {
    Snapshot {
        requested: 0,
        superseded: 0,
        completed: 0,
        objects: 0,
        supersample_pixels: 0,
        bake_micros: 0,
        mesh_tessellations: 0,
        mesh_uploads: 0,
        mesh_evictions: 0,
        mesh_cache_bytes: 0,
        mesh_cache_entries: 0,
    }
}
