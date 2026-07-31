//! Debug-only counters for repeatable hybrid-canvas measurements.

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub requested: u64,
    pub superseded: u64,
    pub completed: u64,
    pub objects: u64,
    pub supersample_pixels: u64,
    pub bake_micros: u64,
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

    pub fn snapshot() -> Snapshot {
        Snapshot {
            requested: REQUESTED.load(Ordering::Relaxed),
            superseded: SUPERSEDED.load(Ordering::Relaxed),
            completed: COMPLETED.load(Ordering::Relaxed),
            objects: OBJECTS.load(Ordering::Relaxed),
            supersample_pixels: PIXELS.load(Ordering::Relaxed),
            bake_micros: MICROS.load(Ordering::Relaxed),
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
pub fn snapshot() -> Snapshot {
    Snapshot {
        requested: 0,
        superseded: 0,
        completed: 0,
        objects: 0,
        supersample_pixels: 0,
        bake_micros: 0,
    }
}
