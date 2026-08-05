use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};

use super::mesh::VectorMesh;
use crate::core::vector::object::VectorObjectData;
use crate::core::vector::style::Paint;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GeometryKey {
    pub document_id: u64,
    pub layer_id: u32,
    pub geometry_fingerprint: u64,
}

pub fn geometry_fingerprint(object: &VectorObjectData) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (object.path.fill_rule as u8).hash(&mut h);
    for contour in &object.path.contours {
        contour.closed.hash(&mut h);
        contour.nodes.len().hash(&mut h);
        for node in &contour.nodes {
            for p in [Some(node.anchor), node.in_handle, node.out_handle] {
                match p {
                    Some(p) => {
                        true.hash(&mut h);
                        p.x.to_bits().hash(&mut h);
                        p.y.to_bits().hash(&mut h);
                    }
                    None => false.hash(&mut h),
                }
            }
        }
    }
    let stroke = object.style.stroke_style;
    matches!(object.style.fill, Paint::None).hash(&mut h);
    matches!(object.style.stroke, Paint::None).hash(&mut h);
    stroke.width.to_bits().hash(&mut h);
    (stroke.cap as u8).hash(&mut h);
    (stroke.join as u8).hash(&mut h);
    stroke.miter_limit.to_bits().hash(&mut h);
    stroke.dash.len.hash(&mut h);
    stroke.dash.offset.to_bits().hash(&mut h);
    for value in stroke.dash.as_slice() {
        value.to_bits().hash(&mut h);
    }
    (stroke.start_arrow.kind as u8).hash(&mut h);
    stroke.start_arrow.size.to_bits().hash(&mut h);
    (stroke.end_arrow.kind as u8).hash(&mut h);
    stroke.end_arrow.size.to_bits().hash(&mut h);
    h.finish()
}

/// Value-agnostic byte-budgeted LRU bookkeeper. It only tracks keys, their byte
/// sizes and recency; the owner keeps the actual values in a parallel map and
/// drops them when [`ByteLru::insert`]/[`ByteLru::touch_evict`] hand back the
/// evicted keys. Kept value-free so the eviction policy is unit-testable with no
/// GPU (the GPU mesh cache stores real `wgpu::Buffer`s that need a device).
#[derive(Debug)]
pub struct ByteLru<K: Copy + Eq + Hash> {
    budget: usize,
    bytes: usize,
    sizes: HashMap<K, usize>,
    lru: VecDeque<K>,
    evictions: u64,
}

impl<K: Copy + Eq + Hash> ByteLru<K> {
    pub fn new(byte_budget: usize) -> Self {
        Self {
            budget: byte_budget.max(1),
            bytes: 0,
            sizes: HashMap::new(),
            lru: VecDeque::new(),
            evictions: 0,
        }
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn len(&self) -> usize {
        self.sizes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sizes.is_empty()
    }

    pub fn evictions(&self) -> u64 {
        self.evictions
    }

    pub fn contains(&self, key: &K) -> bool {
        self.sizes.contains_key(key)
    }

    /// Mark `key` most-recently-used (no-op if absent).
    pub fn touch(&mut self, key: &K) {
        if self.sizes.contains_key(key) {
            self.lru.retain(|k| k != key);
            self.lru.push_back(*key);
        }
    }

    /// Register (or resize) `key`, mark it most-recently-used, then evict the
    /// least-recently-used keys until the budget holds. Never evicts a key in
    /// `protect` (the current frame's working set) so a mesh needed *this* frame
    /// is never dropped mid-frame. Returns the evicted keys so the owner can drop
    /// the corresponding values.
    pub fn insert(&mut self, key: K, bytes: usize, protect: &dyn Fn(&K) -> bool) -> Vec<K> {
        if let Some(old) = self.sizes.insert(key, bytes) {
            self.bytes = self.bytes.saturating_sub(old);
            self.lru.retain(|k| *k != key);
        }
        self.bytes += bytes;
        self.lru.push_back(key);
        // The just-inserted key is always protected from its own eviction — evicting
        // what we just admitted would be pathological (and it is always in the
        // caller's current-frame working set anyway).
        self.evict(&|k| *k == key || protect(k))
    }

    fn evict(&mut self, protect: &dyn Fn(&K) -> bool) -> Vec<K> {
        let mut evicted = Vec::new();
        // Walk oldest-first; skip protected keys but leave them in place so their
        // recency is unchanged. Stop when the budget holds or nothing is evictable.
        let mut scanned = 0usize;
        while self.bytes > self.budget && scanned < self.lru.len() {
            let candidate = self.lru[scanned];
            if protect(&candidate) {
                scanned += 1;
                continue;
            }
            self.lru.remove(scanned);
            if let Some(sz) = self.sizes.remove(&candidate) {
                self.bytes = self.bytes.saturating_sub(sz);
            }
            self.evictions += 1;
            evicted.push(candidate);
        }
        evicted
    }

    /// Drop every key matching `pred`, returning them (e.g. a closed document).
    pub fn drain_matching(&mut self, pred: impl Fn(&K) -> bool) -> Vec<K> {
        let keys: Vec<K> = self.sizes.keys().copied().filter(|k| pred(k)).collect();
        for key in &keys {
            if let Some(sz) = self.sizes.remove(key) {
                self.bytes = self.bytes.saturating_sub(sz);
            }
            self.lru.retain(|k| k != key);
        }
        keys
    }

    pub fn clear(&mut self) {
        self.bytes = 0;
        self.sizes.clear();
        self.lru.clear();
    }
}

pub struct MeshCache {
    budget: usize,
    bytes: usize,
    entries: HashMap<GeometryKey, VectorMesh>,
    lru: VecDeque<GeometryKey>,
}

impl MeshCache {
    pub fn new(byte_budget: usize) -> Self {
        Self {
            budget: byte_budget,
            bytes: 0,
            entries: HashMap::new(),
            lru: VecDeque::new(),
        }
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn get(&mut self, key: &GeometryKey) -> Option<&VectorMesh> {
        if self.entries.contains_key(key) {
            self.lru.retain(|k| k != key);
            self.lru.push_back(*key);
        }
        self.entries.get(key)
    }

    pub fn insert(&mut self, key: GeometryKey, mesh: VectorMesh) {
        if let Some(old) = self.entries.remove(&key) {
            self.bytes = self.bytes.saturating_sub(old.byte_len());
            self.lru.retain(|k| *k != key);
        }
        self.bytes += mesh.byte_len();
        self.entries.insert(key, mesh);
        self.lru.push_back(key);
        while self.bytes > self.budget {
            let Some(oldest) = self.lru.pop_front() else {
                break;
            };
            if let Some(old) = self.entries.remove(&oldest) {
                self.bytes = self.bytes.saturating_sub(old.byte_len());
            }
        }
    }

    pub fn clear_document(&mut self, document_id: u64) {
        let keys: Vec<_> = self
            .entries
            .keys()
            .copied()
            .filter(|k| k.document_id == document_id)
            .collect();
        for key in keys {
            if let Some(mesh) = self.entries.remove(&key) {
                self.bytes = self.bytes.saturating_sub(mesh.byte_len());
            }
            self.lru.retain(|k| *k != key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpu::vector::mesh::VectorVertex;

    fn mesh(vertices: usize) -> VectorMesh {
        VectorMesh {
            vertices: vec![VectorVertex { position: [0.0; 2] }; vertices],
            indices: vec![0; vertices],
            ..Default::default()
        }
    }

    fn never_protect(_: &u32) -> bool {
        false
    }

    #[test]
    fn byte_lru_evicts_oldest_first() {
        let mut lru = ByteLru::new(20);
        assert!(lru.insert(1, 10, &never_protect).is_empty());
        assert!(lru.insert(2, 10, &never_protect).is_empty());
        // Third insert overflows; key 1 (oldest) is evicted.
        assert_eq!(lru.insert(3, 10, &never_protect), vec![1]);
        assert!(!lru.contains(&1));
        assert!(lru.contains(&2) && lru.contains(&3));
        assert_eq!(lru.bytes(), 20);
        assert_eq!(lru.evictions(), 1);
    }

    #[test]
    fn byte_lru_touch_changes_victim() {
        let mut lru = ByteLru::new(20);
        lru.insert(1, 10, &never_protect);
        lru.insert(2, 10, &never_protect);
        lru.touch(&1); // 1 is now newest, so 2 becomes the victim.
        assert_eq!(lru.insert(3, 10, &never_protect), vec![2]);
        assert!(lru.contains(&1) && lru.contains(&3));
        assert!(!lru.contains(&2));
    }

    #[test]
    fn byte_lru_never_evicts_protected_key() {
        // Budget only fits two, but both existing keys are protected: the new key
        // is admitted and the budget is briefly exceeded rather than dropping a
        // key the current frame still needs.
        let mut lru = ByteLru::new(20);
        lru.insert(1, 10, &never_protect);
        lru.insert(2, 10, &never_protect);
        let protect = |k: &u32| *k == 1 || *k == 2;
        assert!(lru.insert(3, 10, &protect).is_empty());
        assert!(lru.contains(&1) && lru.contains(&2) && lru.contains(&3));
        assert_eq!(lru.bytes(), 30);
        // Once unprotected, the next insert trims back down, oldest-first.
        assert_eq!(lru.insert(4, 0, &never_protect), vec![1]);
        assert_eq!(lru.bytes(), 20);
    }

    #[test]
    fn byte_lru_drain_matching_frees_bytes() {
        let mut lru = ByteLru::new(1000);
        lru.insert(10, 5, &never_protect);
        lru.insert(11, 5, &never_protect);
        lru.insert(20, 5, &never_protect);
        let drained = lru.drain_matching(|k| *k < 20);
        assert_eq!(drained.len(), 2);
        assert!(lru.contains(&20) && !lru.contains(&10));
        assert_eq!(lru.bytes(), 5);
    }

    #[test]
    fn evicts_lru_by_byte_budget() {
        let unit = mesh(3).byte_len();
        let mut cache = MeshCache::new(unit * 2);
        let key = |layer_id| GeometryKey {
            document_id: 1,
            layer_id,
            geometry_fingerprint: 1,
        };
        cache.insert(key(1), mesh(3));
        cache.insert(key(2), mesh(3));
        assert!(cache.get(&key(1)).is_some());
        cache.insert(key(3), mesh(3));
        assert!(cache.get(&key(1)).is_some());
        assert!(cache.get(&key(2)).is_none());
        assert!(cache.bytes() <= unit * 2);
    }

    #[test]
    fn transform_and_paint_do_not_change_geometry_key() {
        use crate::core::vector::affine::AffineTransform;
        use crate::core::vector::color::ColorValue;
        use crate::core::vector::object::VectorObjectData;
        use crate::core::vector::path::PathData;
        use crate::core::vector::style::{Paint, VectorStyle};
        let mut object = VectorObjectData::from_path(PathData::default());
        let first = geometry_fingerprint(&object);
        object.transform = AffineTransform::translate(10.0, 20.0);
        object.style.fill = Paint::Solid(ColorValue::WHITE);
        object.style.opacity = 0.5;
        assert_eq!(first, geometry_fingerprint(&object));
        object.style.stroke_style.width = 4.0;
        assert_ne!(first, geometry_fingerprint(&object));
        let width_key = geometry_fingerprint(&object);
        object.style.stroke_style.end_arrow.kind = crate::core::vector::style::ArrowHead::Triangle;
        assert_ne!(width_key, geometry_fingerprint(&object));
        let arrow_key = geometry_fingerprint(&object);
        object.style.stroke_style.end_arrow.size = 7.0;
        assert_ne!(arrow_key, geometry_fingerprint(&object));
        let _ = VectorStyle::default();
    }
}
