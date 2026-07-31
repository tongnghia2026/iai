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
    h.finish()
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
        let _ = VectorStyle::default();
    }
}
