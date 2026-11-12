//! A picture placed into a layer at a finer grain than the page: the layer's
//! own pixels show it at the page's resolution, and files written from the
//! document (SVG, PDF) carry these pixels instead, so a small picture
//! printed large stays sharp. Once the layer's pixels are changed (painted
//! over, scaled), they no longer match `sig` and the layer is written as
//! the pixels it has.

use std::hash::{Hash, Hasher};

/// The finer picture of a layer.
#[derive(Clone, Debug, PartialEq)]
pub struct PlacedImage {
    /// RGBA, `width × height`, straight alpha.
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// Where it lies, in the layer's own pixels: left, top, width, height.
    pub rect: [f32; 4],
    /// The layer's pixels when it was placed ([`pixels_sig`]).
    pub sig: u64,
}

/// A fingerprint of a layer's pixels.
pub fn pixels_sig(rgba: &[u8], width: u32, height: u32) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (width, height).hash(&mut h);
    rgba.hash(&mut h);
    h.finish()
}

impl PlacedImage {
    /// Whether `layer` still shows this picture as it was placed.
    pub fn still_in(&self, layer: &crate::layer::Layer) -> bool {
        pixels_sig(&layer.flatten_tiles(), layer.width, layer.height) == self.sig
    }

    /// Where it lies on the page of `layer`: left, top, width, height.
    pub fn on_page(&self, layer: &crate::layer::Layer) -> [f32; 4] {
        [
            layer.offset.0 as f32 + self.rect[0],
            layer.offset.1 as f32 + self.rect[1],
            self.rect[2],
            self.rect[3],
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_layer_painted_over_no_longer_shows_its_placed_picture() {
        let px = vec![200u8; 4 * 4 * 4];
        let mut layer = crate::layer::Layer::from_rgba(1, "a", px.clone(), 4, 4);
        let placed = PlacedImage {
            rgba: vec![9u8; 16 * 16 * 4],
            width: 16,
            height: 16,
            rect: [0.0, 0.0, 4.0, 4.0],
            sig: pixels_sig(&px, 4, 4),
        };
        assert!(placed.still_in(&layer));
        layer.offset = (10, 20);
        assert!(placed.still_in(&layer));
        assert_eq!(placed.on_page(&layer), [10.0, 20.0, 4.0, 4.0]);
        let mut other = px.clone();
        other[0] = 1;
        let painted = crate::layer::Layer::from_rgba(1, "a", other, 4, 4);
        assert!(!placed.still_in(&painted));
    }
}
