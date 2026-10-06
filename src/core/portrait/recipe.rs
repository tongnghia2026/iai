//! What a "Chân dung" layer was made with, kept on the layer so Chỉnh chân
//! dung can reopen it: the sliders, which faces were on, the masks painted
//! with "Tô vùng" (skin as painted, before an edited brow takes its share)
//! and the selection the analysis kept to. The analysis itself
//! is not kept; reopening runs it again and lays the painted masks over it.

use super::analysis::{Clip, PortraitModel};
use super::effects::{FaceEdits, PortraitSettings};
use super::geometry::Region;

/// A painted mask over its region of the image.
#[derive(Clone, Debug)]
pub struct SavedMask {
    pub region: Region,
    pub mask: Vec<u8>,
}

impl SavedMask {
    /// This mask laid over `analysed` (the mask of `region`): painted values
    /// where the two regions meet, the analysis elsewhere.
    pub fn onto(&self, region: Region, analysed: &[u8]) -> Vec<u8> {
        let mut out = analysed.to_vec();
        let (s, r) = (self.region, region);
        if self.mask.len() != s.len() || out.len() != r.len() {
            return out;
        }
        let (x0, y0) = (s.x.max(r.x), s.y.max(r.y));
        let (x1, y1) = ((s.x + s.w).min(r.x + r.w), (s.y + s.h).min(r.y + r.h));
        if x1 <= x0 || y1 <= y0 {
            return out;
        }
        let n = (x1 - x0) as usize;
        for y in y0..y1 {
            let from = ((y - s.y) * s.w + x0 - s.x) as usize;
            let to = ((y - r.y) * r.w + x0 - r.x) as usize;
            out[to..to + n].copy_from_slice(&self.mask[from..from + n]);
        }
        out
    }
}

/// One face as it was applied: where it sat (to find it again), whether it
/// was on, and its painted masks, if any.
#[derive(Clone, Debug)]
pub struct SavedFace {
    pub centre: [f32; 2],
    pub extent: f32,
    pub enabled: bool,
    pub skin: Option<SavedMask>,
    pub hair: Option<SavedMask>,
    pub brows: Option<SavedMask>,
    pub clothes: Option<SavedMask>,
}

#[derive(Clone, Debug)]
pub struct PortraitRecipe {
    /// Id of the photo layer that was analysed, and its size.
    pub source: u32,
    pub source_size: (u32, u32),
    pub settings: PortraitSettings,
    pub clip: Option<Clip>,
    pub faces: Vec<SavedFace>,
    /// Identity of the layer's pixels as the retouch left them
    /// (`TileMap::content_hash`): a layer worked on since is no longer what
    /// the recipe makes. `None` in a recipe saved before this was kept.
    pub made: Option<u64>,
}

/// A face of a new analysis with what the recipe kept for it: on or off,
/// and its painted masks over the new regions. The clothes are found only
/// when asked for, so their mask comes as it was saved, to lay over them
/// then.
pub struct RestoredFace {
    pub enabled: bool,
    pub skin: Option<Vec<u8>>,
    pub hair: Option<Vec<u8>>,
    pub brows: Option<Vec<u8>>,
    pub clothes: Option<SavedMask>,
}

/// Faces further apart than this, in face sizes, are different faces.
const SAME_FACE: f32 = 0.35;

impl PortraitRecipe {
    pub fn new(
        source: u32,
        source_size: (u32, u32),
        settings: PortraitSettings,
        model: &PortraitModel,
        enabled: &[bool],
        edits: &[FaceEdits],
    ) -> Self {
        let faces = model
            .faces
            .iter()
            .enumerate()
            .map(|(i, face)| {
                let (centre, extent, _) = face.mesh.frame();
                let edit = edits.get(i);
                SavedFace {
                    centre,
                    extent,
                    enabled: enabled.get(i).copied().unwrap_or(true),
                    skin: edit.and_then(|e| e.skin_paint.as_ref()).map(|s| SavedMask {
                        region: face.skin.region(),
                        mask: s.to_vec(),
                    }),
                    hair: edit.and_then(|e| e.hair.as_ref()).map(|h| SavedMask {
                        region: face.hair_region,
                        mask: h.to_vec(),
                    }),
                    brows: edit.and_then(|e| e.brows.as_ref()).map(|b| SavedMask {
                        region: b.region(),
                        mask: b.area().to_vec(),
                    }),
                    clothes: edit.and_then(|e| e.clothes.as_ref()).map(|c| SavedMask {
                        region: c.region,
                        mask: c.mask().to_vec(),
                    }),
                }
            })
            .collect();
        Self {
            source,
            source_size,
            settings,
            clip: model.clip.clone(),
            faces,
            made: None,
        }
    }

    /// Match the faces of a new analysis of the same photo to the saved ones
    /// (nearest centre) and lay their painted masks over the new regions.
    /// A face found only now starts on, as analysed.
    pub fn restore(&self, model: &PortraitModel) -> Vec<RestoredFace> {
        let mut used = vec![false; self.faces.len()];
        model
            .faces
            .iter()
            .map(|face| {
                let (centre, extent, _) = face.mesh.frame();
                let nearest = self
                    .faces
                    .iter()
                    .enumerate()
                    .filter(|(j, _)| !used[*j])
                    .map(|(j, saved)| {
                        let apart = (centre[0] - saved.centre[0])
                            .hypot(centre[1] - saved.centre[1])
                            / extent.max(1.0);
                        (j, apart)
                    })
                    .filter(|(_, apart)| *apart < SAME_FACE)
                    .min_by(|a, b| a.1.total_cmp(&b.1));
                let Some((j, _)) = nearest else {
                    return RestoredFace {
                        enabled: true,
                        skin: None,
                        hair: None,
                        brows: None,
                        clothes: None,
                    };
                };
                used[j] = true;
                let saved = &self.faces[j];
                RestoredFace {
                    enabled: saved.enabled,
                    skin: saved
                        .skin
                        .as_ref()
                        .map(|m| m.onto(face.skin.region(), face.skin.mask())),
                    hair: saved
                        .hair
                        .as_ref()
                        .filter(|_| !face.hair_region.is_empty())
                        .map(|m| m.onto(face.hair_region, face.hair_mask())),
                    brows: saved.brows.as_ref().map(|m| {
                        let analysed = face.brow_layers();
                        m.onto(analysed.region(), analysed.area())
                    }),
                    clothes: saved.clothes.clone(),
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_saved_mask_overrides_the_analysis_only_where_regions_meet() {
        let saved = SavedMask {
            region: Region {
                x: 2,
                y: 1,
                w: 3,
                h: 2,
            },
            mask: vec![9; 6],
        };
        let region = Region {
            x: 0,
            y: 0,
            w: 4,
            h: 3,
        };
        let out = saved.onto(region, &[1; 12]);
        #[rustfmt::skip]
        assert_eq!(out, vec![
            1, 1, 1, 1,
            1, 1, 9, 9,
            1, 1, 9, 9,
        ]);
        // A mask of the wrong size is ignored.
        let broken = SavedMask {
            region: saved.region,
            mask: vec![9; 5],
        };
        assert_eq!(broken.onto(region, &[1; 12]), vec![1; 12]);
    }
}
