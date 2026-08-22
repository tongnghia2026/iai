use rayon::prelude::*;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

static NEXT_TILE_REV: AtomicU64 = AtomicU64::new(100);

#[inline]
pub(crate) fn next_tile_revision() -> u64 {
    NEXT_TILE_REV.fetch_add(1, Ordering::Relaxed)
}

pub const TILE_SIZE: u32 = 256;
pub const TILE_PIXELS: usize = (TILE_SIZE * TILE_SIZE) as usize;
pub const TILE_BYTES: usize = TILE_PIXELS * 4;

/// Ordered 8×8 Bayer threshold in [0,1). Periodic mod 8, and `TILE_SIZE` is a
/// multiple of 8, so tile-local and global coordinates give the same pattern —
/// dithering stays seam-free across tile boundaries.
#[inline]
pub(crate) fn bayer8(x: u32, y: u32) -> f32 {
    const B: [u8; 64] = [
        0, 32, 8, 40, 2, 34, 10, 42, //
        48, 16, 56, 24, 50, 18, 58, 26, //
        12, 44, 4, 36, 14, 46, 6, 38, //
        60, 28, 52, 20, 62, 30, 54, 22, //
        3, 35, 11, 43, 1, 33, 9, 41, //
        51, 19, 59, 27, 49, 17, 57, 25, //
        15, 47, 7, 39, 13, 45, 5, 37, //
        63, 31, 55, 23, 61, 29, 53, 21, //
    ];
    let i = ((y & 7) * 8 + (x & 7)) as usize;
    (B[i] as f32 + 0.5) / 64.0
}

/// Quantize a processed f32 channel [0,1] to u8 with per-channel ordered dithering.
/// The pipeline computes in f32 but the display/document is 8-bit; rounding a smooth
/// gradient posterizes it into flat bands. A sub-LSB ordered offset spreads the
/// rounding boundary so adjacent levels interleave, recovering the extra bits
/// perceptually. The lookup is offset per channel so R/G/B do not share one pattern
/// (which would dither luma only and leave chroma banding).
#[inline]
pub(crate) fn quantize_dither(v: f32, x: u32, y: u32, ch: u32) -> u8 {
    let (ox, oy) = match ch {
        0 => (0u32, 0u32),
        1 => (2, 5),
        _ => (5, 2),
    };
    let t = bayer8(x.wrapping_add(ox), y.wrapping_add(oy));
    (v.clamp(0.0, 1.0) * 255.0 + (t - 0.5))
        .round()
        .clamp(0.0, 255.0) as u8
}

/// Down-convert one 16-bit channel to u8 with the same ordered dither as
/// [`quantize_dither`], so a 16-bit master's 8-bit display mirror (what the GPU
/// atlas uploads) doesn't posterize smooth gradients — RAW skies band badly once
/// Develop's Exposure stretches the truncated 8-bit steps. Preview and commit share
/// one dither character this way. Alpha stays a plain truncation (never dithered).
#[inline]
pub(crate) fn dither16_to_u8(v: u16, x: u32, y: u32, ch: u32) -> u8 {
    quantize_dither(v as f32 / 65535.0, x, y, ch)
}

#[derive(Clone, PartialEq, Eq)]
pub struct Tile {
    pub pixels: Vec<u8>,
    /// Optional 16-bit RGBA master (`TILE_PIXELS*4` samples) for high-bit-depth
    /// documents. `None` = pure 8-bit (the default — zero overhead, behaviour
    /// byte-identical to before). When `Some`, `pixels` mirrors it down-converted
    /// for display/tools; any 8-bit mutation through `get_tile_mut` drops it (the
    /// 16-bit precision is lost on edit until tools become depth-aware — Phase D).
    pub pixels16: Option<Vec<u16>>,
    /// Optional CMYK8 ink planes (`TILE_BYTES` bytes: C,M,Y,K per pixel) — the
    /// ground truth of a CMYK-mode document. When `Some`, `pixels` holds the
    /// document-profile RGB projection of the ink (the display/composite mirror;
    /// its alpha byte is the real layer alpha). Ink-aware writers go through
    /// `get_tile_mut_ink`/`write_ink_region` and re-project the mirror afterwards
    /// (`refresh_mirror_from_ink`); an 8-bit RGB mutation through `get_tile_mut`
    /// drops the plane rather than leave it silently out of sync.
    pub ink: Option<Vec<u8>>,
    pub revision: u64,
}

impl Tile {
    /// Actual heap footprint of this tile (8-bit mirror + optional 16-bit
    /// master + optional CMYK ink planes). The undo history budgets against
    /// this, so 16-bit/CMYK documents' entries are charged their real cost.
    pub fn byte_size(&self) -> usize {
        TILE_BYTES
            + self.pixels16.as_ref().map_or(0, |p| p.len() * 2)
            + self.ink.as_ref().map_or(0, |p| p.len())
    }

    pub fn new_empty() -> Self {
        Self {
            pixels: vec![0u8; TILE_BYTES],
            pixels16: None,
            ink: None,
            revision: 0,
        }
    }

    pub fn new_color(r: u8, g: u8, b: u8, a: u8) -> Self {
        let mut pixels = vec![0u8; TILE_BYTES];
        pixels.par_chunks_mut(4).for_each(|px| {
            px[0] = r;
            px[1] = g;
            px[2] = b;
            px[3] = a;
        });
        Self {
            pixels,
            pixels16: None,
            ink: None,
            revision: 1,
        }
    }

    /// Build a tile from a 16-bit RGBA buffer (`TILE_PIXELS*4` samples): keeps the
    /// 16-bit master and a down-converted 8-bit mirror for display/tools.
    pub fn from_rgba16(px16: Vec<u16>) -> Self {
        let mut pixels = vec![0u8; TILE_BYTES];
        for p in 0..TILE_PIXELS {
            let x = (p % TILE_SIZE as usize) as u32;
            let y = (p / TILE_SIZE as usize) as u32;
            let i = p * 4;
            pixels[i] = dither16_to_u8(px16[i], x, y, 0);
            pixels[i + 1] = dither16_to_u8(px16[i + 1], x, y, 1);
            pixels[i + 2] = dither16_to_u8(px16[i + 2], x, y, 2);
            pixels[i + 3] = (px16[i + 3] >> 8) as u8; // alpha: never dithered
        }
        Self {
            pixels,
            pixels16: Some(px16),
            ink: None,
            revision: 1,
        }
    }

    #[inline]
    pub fn get_pixel(&self, x: u32, y: u32) -> (u8, u8, u8, u8) {
        let i = ((y * TILE_SIZE + x) * 4) as usize;
        (
            self.pixels[i],
            self.pixels[i + 1],
            self.pixels[i + 2],
            self.pixels[i + 3],
        )
    }

    #[inline]
    pub fn set_pixel(&mut self, x: u32, y: u32, r: u8, g: u8, b: u8, a: u8) {
        let i = ((y * TILE_SIZE + x) * 4) as usize;
        self.pixels[i] = r;
        self.pixels[i + 1] = g;
        self.pixels[i + 2] = b;
        self.pixels[i + 3] = a;
    }

    /// 16-bit RGBA sample: from the 16-bit master when present, else the 8-bit
    /// pixel up-converted as `v*257` (so `get_pixel16()/65535.0 == get_pixel()/255.0`
    /// exactly — reading via this path never changes 8-bit results).
    #[inline]
    pub fn get_pixel16(&self, x: u32, y: u32) -> (u16, u16, u16, u16) {
        let i = ((y * TILE_SIZE + x) * 4) as usize;
        if let Some(p) = &self.pixels16 {
            (p[i], p[i + 1], p[i + 2], p[i + 3])
        } else {
            (
                self.pixels[i] as u16 * 257,
                self.pixels[i + 1] as u16 * 257,
                self.pixels[i + 2] as u16 * 257,
                self.pixels[i + 3] as u16 * 257,
            )
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TilePos {
    pub x: i32,
    pub y: i32,
}

impl TilePos {
    #[inline]
    pub fn from_pixel(px: u32, py: u32) -> Self {
        Self {
            x: (px / TILE_SIZE) as i32,
            y: (py / TILE_SIZE) as i32,
        }
    }
}

/// A sparse 2D map of Tiles.
/// Handles Copy-On-Write automatically when mutating tiles.
#[derive(Clone)]
pub struct TileMap {
    pub tiles: HashMap<TilePos, Arc<Tile>>,
    pub width: u32,
    pub height: u32,
}

#[allow(dead_code)]
impl TileMap {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            tiles: HashMap::new(),
            width,
            height,
        }
    }

    /// Compact identity of the current tile contents for detecting whether an
    /// imported PDF base layer still shares its original pixels. Tile revisions
    /// are monotonic; sorting makes the result independent of HashMap order.
    pub fn revision_fingerprint(&self) -> u64 {
        let mut revisions: Vec<(i32, i32, u64)> = self
            .tiles
            .iter()
            .map(|(pos, tile)| (pos.x, pos.y, tile.revision))
            .collect();
        revisions.sort_unstable();

        let mut hash = 0xcbf29ce484222325u64;
        for value in [
            self.width as u64,
            self.height as u64,
            revisions.len() as u64,
        ] {
            hash ^= value;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        for (x, y, revision) in revisions {
            for value in [x as u32 as u64, y as u32 as u64, revision] {
                hash ^= value;
                hash = hash.wrapping_mul(0x100000001b3);
            }
        }
        hash
    }

    /// Box-downsample this map by 2× into a new `TileMap` (ceil dimensions),
    /// averaging each 2×2 source block in premultiplied-alpha space so alpha
    /// edges stay correct. Only the 8-bit display mirror is produced — this is
    /// the compositor's zoomed-out LOD proxy source (uploaded, never edited), so
    /// the 16-bit / CMYK ink planes are intentionally dropped. A destination tile
    /// reads from exactly the 2×2 block of source tiles beneath it (4 map lookups
    /// per tile, not per pixel); absent source tiles read as transparent and a
    /// fully-transparent destination tile is dropped (the result stays sparse).
    pub fn downsample_half(&self) -> TileMap {
        let new_w = ((self.width + 1) / 2).max(1);
        let new_h = ((self.height + 1) / 2).max(1);
        let mut out = TileMap::new(new_w, new_h);
        let cols = (new_w + TILE_SIZE - 1) / TILE_SIZE;
        let rows = (new_h + TILE_SIZE - 1) / TILE_SIZE;
        let rev = NEXT_TILE_REV.fetch_add(1, Ordering::Relaxed);
        let ts = TILE_SIZE as usize;
        let (src_w, src_h) = (self.width, self.height);

        let tile_data: Vec<(TilePos, Tile)> = (0..rows)
            .into_par_iter()
            .flat_map(|ty| {
                (0..cols).into_par_iter().filter_map(move |tx| {
                    // The up-to-4 source tiles whose 512×512 span feeds this dest
                    // tile (dest tile = 256 dest px = 512 src px, tile-aligned).
                    let get = |sc: u32, sr: u32| {
                        self.tiles
                            .get(&TilePos {
                                x: (tx * 2 + sc) as i32,
                                y: (ty * 2 + sr) as i32,
                            })
                            .cloned()
                    };
                    let src = [get(0, 0), get(1, 0), get(0, 1), get(1, 1)];
                    if src.iter().all(Option::is_none) {
                        return None;
                    }
                    // Sample a super-region pixel (0..512) from the right src tile.
                    let sample = |lx: u32, ly: u32| -> (u32, u32, u32, u32) {
                        let idx = usize::from(lx >= TILE_SIZE) + usize::from(ly >= TILE_SIZE) * 2;
                        match &src[idx] {
                            Some(t) => {
                                let (r, g, b, a) = t.get_pixel(lx & 255, ly & 255);
                                (r as u32, g as u32, b as u32, a as u32)
                            }
                            None => (0, 0, 0, 0),
                        }
                    };
                    let mut tile = Tile::new_empty();
                    let mut has = false;
                    let dw = (new_w - tx * TILE_SIZE).min(TILE_SIZE);
                    let dh = (new_h - ty * TILE_SIZE).min(TILE_SIZE);
                    for r in 0..dh {
                        for c in 0..dw {
                            let (mut ar, mut ag, mut ab, mut aa, mut n) = (0u32, 0, 0, 0, 0);
                            for oy in 0..2u32 {
                                for ox in 0..2u32 {
                                    let gx = (tx * TILE_SIZE + c) * 2 + ox;
                                    let gy = (ty * TILE_SIZE + r) * 2 + oy;
                                    if gx < src_w && gy < src_h {
                                        let (pr, pg, pb, pa) = sample(c * 2 + ox, r * 2 + oy);
                                        ar += pr * pa;
                                        ag += pg * pa;
                                        ab += pb * pa;
                                        aa += pa;
                                        n += 1;
                                    }
                                }
                            }
                            if n == 0 {
                                continue;
                            }
                            let a_out = ((aa + n / 2) / n) as u8;
                            let (r_out, g_out, b_out) = if aa > 0 {
                                (
                                    ((ar + aa / 2) / aa) as u8,
                                    ((ag + aa / 2) / aa) as u8,
                                    ((ab + aa / 2) / aa) as u8,
                                )
                            } else {
                                (0, 0, 0)
                            };
                            let di = (r as usize * ts + c as usize) * 4;
                            tile.pixels[di] = r_out;
                            tile.pixels[di + 1] = g_out;
                            tile.pixels[di + 2] = b_out;
                            tile.pixels[di + 3] = a_out;
                            if a_out != 0 {
                                has = true;
                            }
                        }
                    }
                    if !has {
                        return None;
                    }
                    tile.revision = rev;
                    Some((
                        TilePos {
                            x: tx as i32,
                            y: ty as i32,
                        },
                        tile,
                    ))
                })
            })
            .collect();
        for (pos, tile) in tile_data {
            out.tiles.insert(pos, Arc::new(tile));
        }
        out
    }

    /// Create a solid-color TileMap. Every cell shares one Arc<Tile> (COW), so it
    /// allocates O(1 tile) instead of N. `new_black`/`new_white` are color wrappers.
    pub fn new_solid(width: u32, height: u32, r: u8, g: u8, b: u8, a: u8) -> Self {
        let mut map = Self::new(width, height);
        let default_tile = Arc::new(Tile::new_color(r, g, b, a));
        let cols = (width + TILE_SIZE - 1) / TILE_SIZE;
        let rows = (height + TILE_SIZE - 1) / TILE_SIZE;
        for y in 0..rows {
            for x in 0..cols {
                map.tiles.insert(
                    TilePos {
                        x: x as i32,
                        y: y as i32,
                    },
                    Arc::clone(&default_tile),
                );
            }
        }
        map
    }

    pub fn new_black(width: u32, height: u32) -> Self {
        Self::new_solid(width, height, 0, 0, 0, 255)
    }

    pub fn new_white(width: u32, height: u32) -> Self {
        Self::new_solid(width, height, 255, 255, 255, 255)
    }

    pub fn from_rgba(pixels: &[u8], width: u32, height: u32) -> Self {
        let mut map = Self::new(width, height);
        let cols = (width + TILE_SIZE - 1) / TILE_SIZE;
        let rows = (height + TILE_SIZE - 1) / TILE_SIZE;

        let rev = NEXT_TILE_REV.fetch_add(1, Ordering::Relaxed);

        let tile_data: Vec<(TilePos, Tile)> = (0..rows)
            .into_par_iter()
            .flat_map(|ty| {
                (0..cols).into_par_iter().map(move |tx| {
                    let pos = TilePos {
                        x: tx as i32,
                        y: ty as i32,
                    };
                    let mut tile = Tile::new_empty();

                    let start_y = ty * TILE_SIZE;
                    let start_x = tx * TILE_SIZE;

                    for r in 0..TILE_SIZE {
                        let py = start_y + r;
                        if py >= height {
                            break;
                        }

                        for c in 0..TILE_SIZE {
                            let px = start_x + c;
                            if px >= width {
                                break;
                            }

                            let si = ((py * width + px) * 4) as usize;
                            let di = ((r * TILE_SIZE + c) * 4) as usize;
                            if si + 3 < pixels.len() {
                                tile.pixels[di..di + 4].copy_from_slice(&pixels[si..si + 4]);
                            }
                        }
                    }
                    (pos, tile)
                })
            })
            .collect();

        for (pos, mut tile) in tile_data {
            tile.revision = rev;
            map.tiles.insert(pos, Arc::new(tile));
        }

        map
    }

    /// Build a TileMap from a 16-bit RGBA buffer (`width*height*4` samples). Each
    /// tile keeps its 16-bit master plus a down-converted 8-bit mirror, so the
    /// existing 8-bit display/tool paths keep working unchanged.
    pub fn from_rgba16(px16: &[u16], width: u32, height: u32) -> Self {
        let mut map = Self::new(width, height);
        let cols = (width + TILE_SIZE - 1) / TILE_SIZE;
        let rows = (height + TILE_SIZE - 1) / TILE_SIZE;
        let rev = NEXT_TILE_REV.fetch_add(1, Ordering::Relaxed);

        let tile_data: Vec<(TilePos, Tile)> = (0..rows)
            .into_par_iter()
            .flat_map(|ty| {
                (0..cols).into_par_iter().map(move |tx| {
                    let pos = TilePos {
                        x: tx as i32,
                        y: ty as i32,
                    };
                    let mut buf = vec![0u16; TILE_PIXELS * 4];
                    let start_y = ty * TILE_SIZE;
                    let start_x = tx * TILE_SIZE;
                    for r in 0..TILE_SIZE {
                        let py = start_y + r;
                        if py >= height {
                            break;
                        }
                        for c in 0..TILE_SIZE {
                            let px = start_x + c;
                            if px >= width {
                                break;
                            }
                            let si = ((py * width + px) * 4) as usize;
                            let di = ((r * TILE_SIZE + c) * 4) as usize;
                            if si + 3 < px16.len() {
                                buf[di..di + 4].copy_from_slice(&px16[si..si + 4]);
                            }
                        }
                    }
                    (pos, Tile::from_rgba16(buf))
                })
            })
            .collect();

        for (pos, mut tile) in tile_data {
            tile.revision = rev;
            map.tiles.insert(pos, Arc::new(tile));
        }
        map
    }

    /// True when every present tile carries a 16-bit master (the map can be
    /// flattened at full 16-bit precision without up-conversion).
    pub fn has_hdr(&self) -> bool {
        !self.tiles.is_empty() && self.tiles.values().all(|t| t.pixels16.is_some())
    }

    /// Promote to 16-bit: give every tile a 16-bit master up-converted from its
    /// 8-bit pixels (`v*257`). Used by Image ▸ Mode ▸ 16 Bits/Channel.
    pub fn promote_to_hdr(&mut self) {
        for tile in self.tiles.values_mut() {
            if tile.pixels16.is_none() {
                let t = Arc::make_mut(tile);
                t.pixels16 = Some(t.pixels.iter().map(|&v| (v as u16) * 257).collect());
            }
        }
    }

    /// Drop all 16-bit masters (back to 8-bit). Used by Image ▸ Mode ▸ 8 Bits/Channel.
    pub fn drop_hdr(&mut self) {
        for tile in self.tiles.values_mut() {
            if tile.pixels16.is_some() {
                Arc::make_mut(tile).pixels16 = None;
            }
        }
    }

    /// Rebuild 16-bit masters for tiles a paint stroke touched (their masters
    /// were dropped by `get_tile_mut`), keeping full precision on pixels the
    /// stroke left alone. For each sample in a master-less tile: if it still
    /// matches the pre-stroke 8-bit mirror in `before`, restore the original
    /// 16-bit value; otherwise up-convert the painted 8-bit value (`v*257`).
    /// Tiles that already hold a master (untouched) are skipped. Used at
    /// `end_stroke` on a 16-bit document so painting stays non-destructive to
    /// the surrounding 16-bit data.
    pub fn repromote_after_paint(&mut self, before: &TileMap) {
        for (pos, tile) in self.tiles.iter_mut() {
            if tile.pixels16.is_some() {
                continue;
            }
            let before_tile = before.tiles.get(pos);
            let before16 = before_tile.and_then(|b| b.pixels16.as_ref());
            let before8 = before_tile.map(|b| &b.pixels);
            let t = Arc::make_mut(tile);
            let mut master = vec![0u16; TILE_BYTES];
            for i in 0..TILE_BYTES {
                let cur = t.pixels[i];
                master[i] = match (before16, before8) {
                    (Some(b16), Some(b8)) if b8[i] == cur => b16[i],
                    _ => cur as u16 * 257,
                };
            }
            t.pixels16 = Some(master);
        }
    }

    /// Flatten to a 16-bit RGBA buffer (`width*height*4` samples). Tiles with a
    /// 16-bit master contribute full precision; 8-bit-only tiles are up-converted
    /// (`v*257`); absent tiles are transparent. Returns an empty vec on overflow.
    pub fn flatten16(&self) -> Vec<u16> {
        let Some(len) = (self.width as u64)
            .checked_mul(self.height as u64)
            .and_then(|n| n.checked_mul(4))
            .and_then(|n| usize::try_from(n).ok())
        else {
            return Vec::new();
        };
        let mut out = vec![0u16; len];
        let w = self.width;
        let h = self.height;
        let cols = (w + TILE_SIZE - 1) / TILE_SIZE;

        out.par_chunks_mut((w * 4) as usize)
            .enumerate()
            .for_each(|(y, row)| {
                let py = y as u32;
                if py >= h {
                    return;
                }
                let ty = py / TILE_SIZE;
                let ty_rem = py % TILE_SIZE;
                for tx in 0..cols {
                    let start_x = tx * TILE_SIZE;
                    let end_x = (start_x + TILE_SIZE).min(w);
                    if start_x >= end_x {
                        continue;
                    }
                    let pos = TilePos {
                        x: tx as i32,
                        y: ty as i32,
                    };
                    let Some(tile) = self.tiles.get(&pos) else {
                        continue;
                    };
                    let n = ((end_x - start_x) * 4) as usize;
                    let dst0 = (start_x * 4) as usize;
                    let src0 = ((ty_rem * TILE_SIZE) * 4) as usize;
                    if let Some(p16) = &tile.pixels16 {
                        if src0 + n <= p16.len() && dst0 + n <= row.len() {
                            row[dst0..dst0 + n].copy_from_slice(&p16[src0..src0 + n]);
                        }
                    } else if src0 + n <= tile.pixels.len() && dst0 + n <= row.len() {
                        for k in 0..n {
                            row[dst0 + k] = (tile.pixels[src0 + k] as u16) * 257;
                        }
                    }
                }
            });
        out
    }

    /// Get a tile read-only (fast).
    pub fn get_tile(&self, pos: TilePos) -> Option<Arc<Tile>> {
        self.tiles.get(&pos).cloned()
    }

    /// Get a tile for mutation (copy-on-write if the tile is shared).
    pub fn get_tile_mut(&mut self, pos: TilePos) -> &mut Tile {
        let arc_tile = self
            .tiles
            .entry(pos)
            .or_insert_with(|| Arc::new(Tile::new_empty()));
        let t = Arc::make_mut(arc_tile);
        t.revision = next_tile_revision();
        // An 8-bit mutation invalidates the 16-bit master (precision lost).
        t.pixels16 = None;
        // …and the CMYK ink plane: dropping it fails loudly (the plate reads as
        // no ink) instead of leaving stale ink silently disagreeing with the
        // mirror. CMYK-legal writers use get_tile_mut_ink / write_ink_region.
        t.ink = None;
        t
    }

    /// Get a tile for an ink-plane mutation: copy-on-write like `get_tile_mut`,
    /// but KEEPS `ink` (creating a zeroed plane if missing — correct for fresh
    /// tiles, whose mirror is transparent) and leaves `pixels16` alone. The
    /// caller writes ink and then re-projects the mirror for the touched rect
    /// via [`Self::refresh_mirror_from_ink`].
    pub fn get_tile_mut_ink(&mut self, pos: TilePos) -> &mut Tile {
        let arc_tile = self
            .tiles
            .entry(pos)
            .or_insert_with(|| Arc::new(Tile::new_empty()));
        let t = Arc::make_mut(arc_tile);
        t.revision = next_tile_revision();
        if t.ink.is_none() {
            t.ink = Some(vec![0u8; TILE_BYTES]);
        }
        t
    }

    /// Assign every tile a fresh monotonic revision so the GPU tile atlas treats
    /// them as new content and re-uploads. Needed for rebuilt *synthetic* layers
    /// (e.g. an effected layer-group's flattened subtree) that reuse the same
    /// `(layer_id, pos)` atlas key each frame: without a changing revision the
    /// atlas serves the stale cached tile and child edits never appear.
    pub fn bump_all_revisions(&mut self) {
        for tile in self.tiles.values_mut() {
            let rev = next_tile_revision();
            Arc::make_mut(tile).revision = rev;
        }
    }

    /// Give restored/replayed tiles fresh globally unique revisions wherever
    /// `other` contains different tile storage. Undo must not put an old local
    /// revision back into the GPU atlas: a later edit can otherwise reach the
    /// same revision number as stale cached pixels and skip the required upload.
    pub fn bump_changed_revisions(&mut self, other: &Self) {
        for (pos, tile) in &mut self.tiles {
            let changed = other
                .tiles
                .get(pos)
                .is_none_or(|other_tile| !Arc::ptr_eq(tile, other_tile));
            if changed {
                let rev = next_tile_revision();
                Arc::make_mut(tile).revision = rev;
            }
        }
    }

    pub fn write_region(&mut self, x0: u32, y0: u32, w: u32, h: u32, pixels: &[u8]) {
        let x1 = x0 + w;
        let y1 = y0 + h;

        let tx0 = x0 / TILE_SIZE;
        let ty0 = y0 / TILE_SIZE;
        let tx1 = (x1 - 1) / TILE_SIZE;
        let ty1 = (y1 - 1) / TILE_SIZE;

        for ty in ty0..=ty1 {
            for tx in tx0..=tx1 {
                let pos = TilePos {
                    x: tx as i32,
                    y: ty as i32,
                };

                let tile_start_x = tx * TILE_SIZE;
                let tile_start_y = ty * TILE_SIZE;

                let copy_x0 = x0.max(tile_start_x);
                let copy_y0 = y0.max(tile_start_y);
                let copy_x1 = x1.min(tile_start_x + TILE_SIZE);
                let copy_y1 = y1.min(tile_start_y + TILE_SIZE);

                if copy_x1 <= copy_x0 || copy_y1 <= copy_y0 {
                    continue;
                }

                // Keep a sparse destination sparse. Crop/resample writes in
                // tile-sized chunks, including chunks that are completely
                // transparent when a layer has no content in that part of the
                // canvas. Creating a 256x256 allocation for every such chunk
                // turns an otherwise sparse document into O(layers * canvas)
                // memory. An existing tile still has to be written so zeroes can
                // clear it; only a missing destination tile may be skipped.
                if !self.tiles.contains_key(&pos) {
                    let mut all_zero = true;
                    'zero_scan: for py in copy_y0..copy_y1 {
                        let src_row = py - y0;
                        let src_col = copy_x0 - x0;
                        let src_idx = ((src_row * w + src_col) * 4) as usize;
                        let len = ((copy_x1 - copy_x0) * 4) as usize;
                        if pixels[src_idx..src_idx + len].iter().any(|&v| v != 0) {
                            all_zero = false;
                            break 'zero_scan;
                        }
                    }
                    if all_zero {
                        continue;
                    }
                }

                let tile = self.get_tile_mut(pos);
                for py in copy_y0..copy_y1 {
                    let tile_row = py - tile_start_y;
                    let tile_col = copy_x0 - tile_start_x;
                    let tile_idx = ((tile_row * TILE_SIZE + tile_col) * 4) as usize;

                    let src_row = py - y0;
                    let src_col = copy_x0 - x0;
                    let src_idx = ((src_row * w + src_col) * 4) as usize;

                    let len = ((copy_x1 - copy_x0) * 4) as usize;

                    tile.pixels[tile_idx..tile_idx + len]
                        .copy_from_slice(&pixels[src_idx..src_idx + len]);
                }
            }
        }
    }

    /// 16-bit twin of [`Self::write_region`]: write a 16-bit region into the tiles,
    /// keeping each touched tile a 16-bit master and refreshing its ordered-dithered
    /// 8-bit display mirror. Lets a chunked 16-bit composite (large-canvas Flatten /
    /// Stamp Visible) build tiles without a canvas-sized 16-bit buffer.
    pub fn write_region16(&mut self, x0: u32, y0: u32, w: u32, h: u32, px16: &[u16]) {
        let x1 = x0 + w;
        let y1 = y0 + h;
        let tx0 = x0 / TILE_SIZE;
        let ty0 = y0 / TILE_SIZE;
        let tx1 = (x1 - 1) / TILE_SIZE;
        let ty1 = (y1 - 1) / TILE_SIZE;

        for ty in ty0..=ty1 {
            for tx in tx0..=tx1 {
                let pos = TilePos {
                    x: tx as i32,
                    y: ty as i32,
                };
                let tile_start_x = tx * TILE_SIZE;
                let tile_start_y = ty * TILE_SIZE;
                let copy_x0 = x0.max(tile_start_x);
                let copy_y0 = y0.max(tile_start_y);
                let copy_x1 = x1.min(tile_start_x + TILE_SIZE);
                let copy_y1 = y1.min(tile_start_y + TILE_SIZE);
                if copy_x1 <= copy_x0 || copy_y1 <= copy_y0 {
                    continue;
                }

                // See write_region: do not materialize an absent destination
                // tile when this 16-bit write contains only transparent zeroes.
                if !self.tiles.contains_key(&pos) {
                    let mut all_zero = true;
                    'zero_scan: for py in copy_y0..copy_y1 {
                        let src_row = py - y0;
                        let src_col = copy_x0 - x0;
                        let src_idx = ((src_row * w + src_col) * 4) as usize;
                        let len = ((copy_x1 - copy_x0) * 4) as usize;
                        if px16[src_idx..src_idx + len].iter().any(|&v| v != 0) {
                            all_zero = false;
                            break 'zero_scan;
                        }
                    }
                    if all_zero {
                        continue;
                    }
                }

                // Mutable tile that KEEPS its 16-bit master (get_tile_mut drops it).
                let arc = self
                    .tiles
                    .entry(pos)
                    .or_insert_with(|| Arc::new(Tile::new_empty()));
                let tile = Arc::make_mut(arc);
                tile.revision += 1;
                if tile.pixels16.is_none() {
                    tile.pixels16 = Some(tile.pixels.iter().map(|&v| v as u16 * 257).collect());
                }

                for py in copy_y0..copy_y1 {
                    let tile_row = py - tile_start_y;
                    let tile_col = copy_x0 - tile_start_x;
                    let ti = ((tile_row * TILE_SIZE + tile_col) * 4) as usize;
                    let src_row = py - y0;
                    let src_col = copy_x0 - x0;
                    let si = ((src_row * w + src_col) * 4) as usize;
                    let span = (copy_x1 - copy_x0) as usize;

                    {
                        let p16 = tile.pixels16.as_mut().unwrap();
                        p16[ti..ti + span * 4].copy_from_slice(&px16[si..si + span * 4]);
                    }
                    // Refresh the 8-bit mirror (ordered-dithered) for the same span.
                    let master = tile.pixels16.as_ref().unwrap();
                    for i in 0..span {
                        let o = ti + i * 4;
                        let lx = tile_col + i as u32;
                        tile.pixels[o] = dither16_to_u8(master[o], lx, tile_row, 0);
                        tile.pixels[o + 1] = dither16_to_u8(master[o + 1], lx, tile_row, 1);
                        tile.pixels[o + 2] = dither16_to_u8(master[o + 2], lx, tile_row, 2);
                        tile.pixels[o + 3] = (master[o + 3] >> 8) as u8;
                    }
                }
            }
        }
    }

    /// True when any tile carries a CMYK ink plane (i.e. this map belongs to a
    /// CMYK-mode document and has ink content worth carrying through rebuilds).
    pub fn has_any_ink(&self) -> bool {
        self.tiles.values().any(|t| t.ink.is_some())
    }

    /// True when at least one tile is missing its CMYK ink plane. A Path/vector
    /// layer's raster is rebuilt wholesale by `from_rgba` (which produces ink-less
    /// tiles), so this flags a layer whose mirror was just re-rasterized and whose
    /// ink planes must be re-derived. A layer already fully inked returns `false`,
    /// letting [`Canvas::reconcile_path_ink`] skip layers nothing touched instead
    /// of re-encoding every vector layer on every edit.
    pub fn needs_ink_encode(&self) -> bool {
        self.tiles.values().any(|t| t.ink.is_none())
    }

    /// RGB→CMYK convert of this whole map (document mode conversion): every
    /// tile gets an ink plane encoded from its RGB mirror, and the mirror is
    /// re-projected from that ink so it shows what the ink actually reproduces
    /// (an ICC space clips out-of-gamut colours; naive is lossless). Alpha is
    /// untouched; any 16-bit master is dropped (CMYK editing is 8-bit).
    pub fn encode_ink_from_mirror(&mut self, conv: &crate::core::cms::CmykConverter) {
        for tile in self.tiles.values_mut() {
            let t = Arc::make_mut(tile);
            t.revision += 1;
            t.pixels16 = None;
            let mut plane = vec![0u8; TILE_BYTES];
            let mut rgb = vec![[0u8; 3]; TILE_PIXELS];
            for p in 0..TILE_PIXELS {
                let i = p * 4;
                rgb[p] = [t.pixels[i], t.pixels[i + 1], t.pixels[i + 2]];
            }
            {
                let ink_px: &mut [[u8; 4]] = bytemuck::cast_slice_mut(&mut plane);
                conv.rgb_to_cmyk_slice(&rgb, ink_px);
                conv.cmyk_to_rgb_slice(ink_px, &mut rgb);
            }
            for (p, px) in rgb.iter().enumerate() {
                let i = p * 4;
                t.pixels[i] = px[0];
                t.pixels[i + 1] = px[1];
                t.pixels[i + 2] = px[2];
            }
            t.ink = Some(plane);
        }
    }

    /// Drop every ink plane (CMYK→RGB mode conversion): the RGB mirror — which
    /// is the profile projection of the ink — simply becomes the ground truth.
    pub fn drop_ink(&mut self) {
        for tile in self.tiles.values_mut() {
            if tile.ink.is_some() {
                Arc::make_mut(tile).ink = None;
            }
        }
    }

    /// Write a packed CMYK8 region (`w*h*4` bytes) into the tiles' ink planes.
    /// The RGB mirror is NOT touched — callers either wrote it separately (crop
    /// blit) or re-project it afterwards via [`Self::refresh_mirror_from_ink`].
    pub fn write_ink_region(&mut self, x0: u32, y0: u32, w: u32, h: u32, ink: &[u8]) {
        if w == 0 || h == 0 {
            return;
        }
        let x1 = x0 + w;
        let y1 = y0 + h;
        let tx0 = x0 / TILE_SIZE;
        let ty0 = y0 / TILE_SIZE;
        let tx1 = (x1 - 1) / TILE_SIZE;
        let ty1 = (y1 - 1) / TILE_SIZE;

        for ty in ty0..=ty1 {
            for tx in tx0..=tx1 {
                let pos = TilePos {
                    x: tx as i32,
                    y: ty as i32,
                };
                let tile_start_x = tx * TILE_SIZE;
                let tile_start_y = ty * TILE_SIZE;
                let copy_x0 = x0.max(tile_start_x);
                let copy_y0 = y0.max(tile_start_y);
                let copy_x1 = x1.min(tile_start_x + TILE_SIZE);
                let copy_y1 = y1.min(tile_start_y + TILE_SIZE);
                if copy_x1 <= copy_x0 || copy_y1 <= copy_y0 {
                    continue;
                }

                let tile = self.get_tile_mut_ink(pos);
                let plane = tile.ink.as_mut().unwrap();
                for py in copy_y0..copy_y1 {
                    let tile_row = py - tile_start_y;
                    let tile_col = copy_x0 - tile_start_x;
                    let ti = ((tile_row * TILE_SIZE + tile_col) * 4) as usize;
                    let si = (((py - y0) * w + (copy_x0 - x0)) * 4) as usize;
                    let len = ((copy_x1 - copy_x0) * 4) as usize;
                    plane[ti..ti + len].copy_from_slice(&ink[si..si + len]);
                }
            }
        }
    }

    /// Extract a packed CMYK8 region (`w*h*4` bytes) from the tiles' ink planes
    /// into `out`. Tiles without an ink plane (or absent) read as zero ink.
    pub fn extract_ink_region_into(&self, x0: u32, y0: u32, w: u32, h: u32, out: &mut [u8]) {
        if w == 0 || h == 0 {
            return;
        }
        out.fill(0);
        for row in 0..h {
            let py = y0 + row;
            if py >= self.height {
                break;
            }
            let ty = py / TILE_SIZE;
            let ty_rem = py % TILE_SIZE;
            let mut px = x0;
            while px < x0 + w {
                if px >= self.width {
                    break;
                }
                let tx = px / TILE_SIZE;
                let tx_rem = px % TILE_SIZE;
                let copy_w = (TILE_SIZE - tx_rem).min(x0 + w - px).min(self.width - px);
                let pos = TilePos {
                    x: tx as i32,
                    y: ty as i32,
                };
                if let Some(plane) = self.tiles.get(&pos).and_then(|t| t.ink.as_ref()) {
                    let src_i = ((ty_rem * TILE_SIZE + tx_rem) * 4) as usize;
                    let dst_i = ((row * w + (px - x0)) * 4) as usize;
                    let len = (copy_w * 4) as usize;
                    if src_i + len <= plane.len() && dst_i + len <= out.len() {
                        out[dst_i..dst_i + len].copy_from_slice(&plane[src_i..src_i + len]);
                    }
                }
                px += copy_w;
            }
        }
    }

    /// Re-project the RGB mirror from the ink planes for a canvas-space rect
    /// (the alpha byte is left untouched — it is the real layer alpha, not part
    /// of the ink). Call after mutating ink through `get_tile_mut_ink` /
    /// `write_ink_region`. Tiles without an ink plane are skipped.
    pub fn refresh_mirror_from_ink(
        &mut self,
        x0: u32,
        y0: u32,
        w: u32,
        h: u32,
        conv: &crate::core::cms::CmykConverter,
    ) {
        if w == 0 || h == 0 {
            return;
        }
        let x1 = (x0 + w).min(self.width);
        let y1 = (y0 + h).min(self.height);
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        let tx0 = x0 / TILE_SIZE;
        let ty0 = y0 / TILE_SIZE;
        let tx1 = (x1 - 1) / TILE_SIZE;
        let ty1 = (y1 - 1) / TILE_SIZE;

        let mut rgb_row: Vec<[u8; 3]> = Vec::new();
        for ty in ty0..=ty1 {
            for tx in tx0..=tx1 {
                let pos = TilePos {
                    x: tx as i32,
                    y: ty as i32,
                };
                let Some(arc) = self.tiles.get_mut(&pos) else {
                    continue;
                };
                if arc.ink.is_none() {
                    continue;
                }
                let tile_start_x = tx * TILE_SIZE;
                let tile_start_y = ty * TILE_SIZE;
                let copy_x0 = x0.max(tile_start_x);
                let copy_y0 = y0.max(tile_start_y);
                let copy_x1 = x1.min(tile_start_x + TILE_SIZE);
                let copy_y1 = y1.min(tile_start_y + TILE_SIZE);
                if copy_x1 <= copy_x0 || copy_y1 <= copy_y0 {
                    continue;
                }

                let t = Arc::make_mut(arc);
                t.revision += 1;
                let span = (copy_x1 - copy_x0) as usize;
                rgb_row.resize(span, [0u8; 3]);
                for py in copy_y0..copy_y1 {
                    let tile_row = py - tile_start_y;
                    let tile_col = copy_x0 - tile_start_x;
                    let o = ((tile_row * TILE_SIZE + tile_col) * 4) as usize;
                    {
                        let plane = t.ink.as_ref().unwrap();
                        let ink_px: &[[u8; 4]] = bytemuck::cast_slice(&plane[o..o + span * 4]);
                        conv.cmyk_to_rgb_slice(ink_px, &mut rgb_row[..span]);
                    }
                    for (i, rgb) in rgb_row[..span].iter().enumerate() {
                        let d = o + i * 4;
                        t.pixels[d] = rgb[0];
                        t.pixels[d + 1] = rgb[1];
                        t.pixels[d + 2] = rgb[2];
                        // t.pixels[d + 3] (alpha) untouched.
                    }
                }
            }
        }
    }

    /// Apply per-ink LUTs `[C, M, Y, K]` to every ink pixel whose mirror alpha
    /// is non-zero, then re-project the RGB mirror from the edited ink (alpha
    /// untouched) — the ink-native core of CMYK Levels/Curves. `sel` returns
    /// selection coverage for a layer-local pixel; partial coverage lerps
    /// old→new ink. Untouched tiles are left un-cloned (COW-friendly).
    pub fn apply_ink_luts(
        &mut self,
        luts: &[[u8; 256]; 4],
        conv: &crate::core::cms::CmykConverter,
        mut sel: impl FnMut(i64, i64) -> f32,
    ) -> bool {
        let mut any = false;
        let mut rgb = vec![[0u8; 3]; TILE_PIXELS];
        for (pos, arc) in self.tiles.iter_mut() {
            let Some(ink) = arc.ink.as_ref() else {
                continue;
            };
            let base_x = pos.x as i64 * TILE_SIZE as i64;
            let base_y = pos.y as i64 * TILE_SIZE as i64;
            let mut new_ink = ink.clone();
            let mut changed = false;
            for p in 0..TILE_PIXELS {
                let i = p * 4;
                if arc.pixels[i + 3] == 0 {
                    continue;
                }
                let lx = base_x + (p as u32 % TILE_SIZE) as i64;
                let ly = base_y + (p as u32 / TILE_SIZE) as i64;
                let s = sel(lx, ly);
                if s <= 0.001 {
                    continue;
                }
                for (c, lut) in luts.iter().enumerate() {
                    let old = ink[i + c];
                    let lu = lut[old as usize];
                    let nv = if s >= 0.999 {
                        lu
                    } else {
                        (lu as f32 * s + old as f32 * (1.0 - s)).round() as u8
                    };
                    if nv != old {
                        new_ink[i + c] = nv;
                        changed = true;
                    }
                }
            }
            if !changed {
                continue;
            }
            let t = Arc::make_mut(arc);
            t.revision += 1;
            t.pixels16 = None;
            {
                let ink_px: &[[u8; 4]] = bytemuck::cast_slice(&new_ink);
                conv.cmyk_to_rgb_slice(ink_px, &mut rgb);
            }
            for (p, px) in rgb.iter().enumerate() {
                let i = p * 4;
                if t.pixels[i + 3] == 0 {
                    continue;
                }
                t.pixels[i] = px[0];
                t.pixels[i + 1] = px[1];
                t.pixels[i + 2] = px[2];
            }
            t.ink = Some(new_ink);
            any = true;
        }
        any
    }

    /// Get a pixel value (safe; returns [0,0,0,0] outside the allocated area).
    #[inline]
    pub fn get_pixel(&self, x: u32, y: u32) -> (u8, u8, u8, u8) {
        if x >= self.width || y >= self.height {
            return (0, 0, 0, 0);
        }
        let pos = TilePos::from_pixel(x, y);
        if let Some(tile) = self.tiles.get(&pos) {
            tile.get_pixel(x % TILE_SIZE, y % TILE_SIZE)
        } else {
            (0, 0, 0, 0)
        }
    }

    /// 16-bit variant of [`Self::get_pixel`]; missing tiles read as transparent.
    #[inline]
    pub fn get_pixel16(&self, x: u32, y: u32) -> (u16, u16, u16, u16) {
        if x >= self.width || y >= self.height {
            return (0, 0, 0, 0);
        }
        let pos = TilePos::from_pixel(x, y);
        if let Some(tile) = self.tiles.get(&pos) {
            tile.get_pixel16(x % TILE_SIZE, y % TILE_SIZE)
        } else {
            (0, 0, 0, 0)
        }
    }

    /// Set a pixel value (auto-allocates a tile and copy-on-writes if needed).
    #[inline]
    pub fn set_pixel(&mut self, x: u32, y: u32, r: u8, g: u8, b: u8, a: u8) {
        if x >= self.width || y >= self.height {
            return;
        }
        let pos = TilePos::from_pixel(x, y);
        let tile = self.get_tile_mut(pos);
        tile.set_pixel(x % TILE_SIZE, y % TILE_SIZE, r, g, b, a);
    }

    /// Translate all pixels by dx, dy — **BAKE-ONLY OPERATIONS**.
    ///
    /// Tiles are always in layer-local space (0,0 = layer's top-left).
    /// For visual movement (Move tool, arrow-key nudge), MUST use `layer.offset += (dx, dy)`
    /// instead of `TileMap::translate()`. Calling translate() for movement breaks:
    ///   - Undo (TranslateLayerCommand tracks only offset, not tile coords)
    ///   - Layer groups (the offset hierarchy needs its own offset; can't bake tiles)
    ///   - Partial recomposite (dirty rect is computed from offset, not tile coords)
    ///
    /// Valid use cases for `translate()`:
    ///   - Flatten/merge: bake the layer offset into tile coords when merging down
    ///   - Crop+resize internal: rebuild the TileMap from the extracted region
    ///
    /// When dx/dy are multiples of TILE_SIZE → O(tiles) key remap (no pixel copy).
    /// When unaligned → pixel-by-pixel rebuild (O(W×H)).
    pub fn translate(&mut self, dx: i32, dy: i32) {
        if dx == 0 && dy == 0 {
            return;
        }

        let ts = TILE_SIZE as i32;
        let tile_dx = dx.div_euclid(ts);
        let tile_dy = dy.div_euclid(ts);
        let rem_x = dx.rem_euclid(ts) as u32;
        let rem_y = dy.rem_euclid(ts) as u32;

        if rem_x == 0 && rem_y == 0 {
            let old = std::mem::take(&mut self.tiles);
            let w = self.width as i32;
            let h = self.height as i32;
            for (pos, tile) in old {
                let new_pos = TilePos {
                    x: pos.x + tile_dx,
                    y: pos.y + tile_dy,
                };
                let tx0 = new_pos.x * ts;
                let ty0 = new_pos.y * ts;
                if tx0 < w && ty0 < h && tx0 + ts > 0 && ty0 + ts > 0 {
                    self.tiles.insert(new_pos, tile);
                }
            }
        } else {
            let old_tiles = std::mem::take(&mut self.tiles);
            let mut new_tiles = std::collections::HashMap::new();
            let w = self.width as i32;
            let h = self.height as i32;

            for (pos, tile) in old_tiles {
                let tx = pos.x * ts;
                let ty = pos.y * ts;
                for py in 0..TILE_SIZE {
                    for px in 0..TILE_SIZE {
                        let (r, g, b, a) = tile.get_pixel(px, py);
                        if a == 0 {
                            continue;
                        }
                        let nx = tx + px as i32 + dx;
                        let ny = ty + py as i32 + dy;
                        if nx >= 0 && nx < w && ny >= 0 && ny < h {
                            let np = TilePos::from_pixel(nx as u32, ny as u32);
                            let dest = new_tiles
                                .entry(np)
                                .or_insert_with(|| Arc::new(Tile::new_empty()));
                            let t = Arc::make_mut(dest);
                            t.revision += 1;
                            let lx = nx as u32 % TILE_SIZE;
                            let ly = ny as u32 % TILE_SIZE;
                            t.set_pixel(lx, ly, r, g, b, a);
                            // Carry the CMYK ink byte-for-byte with its pixel.
                            if let Some(src_ink) = tile.ink.as_ref() {
                                let si = ((py * TILE_SIZE + px) * 4) as usize;
                                let di = ((ly * TILE_SIZE + lx) * 4) as usize;
                                let dst_ink = t.ink.get_or_insert_with(|| vec![0u8; TILE_BYTES]);
                                dst_ink[di..di + 4].copy_from_slice(&src_ink[si..si + 4]);
                            }
                        }
                    }
                }
            }
            self.tiles = new_tiles;
        }
    }

    /// Temporary back-compat shim for the old API (exports a flat array).
    /// Will be phased out and replaced by GPU tile upload.
    pub fn flatten(&self) -> Vec<u8> {
        let Some(len) = (self.width as u64)
            .checked_mul(self.height as u64)
            .and_then(|n| n.checked_mul(4))
            .and_then(|n| usize::try_from(n).ok())
        else {
            return Vec::new();
        };
        let mut pixels = vec![0u8; len];

        let w = self.width;
        let h = self.height;
        let tile_map = &self.tiles;

        let cols = (w + TILE_SIZE - 1) / TILE_SIZE;
        let _rows = (h + TILE_SIZE - 1) / TILE_SIZE;

        pixels
            .par_chunks_mut((w * 4) as usize)
            .enumerate()
            .for_each(|(y, row_slice)| {
                let py = y as u32;
                if py >= h {
                    return;
                }
                let ty = py / TILE_SIZE;
                let ty_rem = py % TILE_SIZE;

                for tx in 0..cols {
                    let start_x = tx * TILE_SIZE;
                    let end_x = (start_x + TILE_SIZE).min(w);
                    if start_x >= end_x {
                        continue;
                    }

                    let pos = TilePos {
                        x: tx as i32,
                        y: ty as i32,
                    };
                    if let Some(tile) = tile_map.get(&pos) {
                        let tile_row_start = ((ty_rem * TILE_SIZE) * 4) as usize;
                        let len = ((end_x - start_x) * 4) as usize;

                        let dst_start = (start_x * 4) as usize;
                        let dst_end = dst_start + len;

                        if tile_row_start + len <= tile.pixels.len() && dst_end <= row_slice.len() {
                            row_slice[dst_start..dst_end].copy_from_slice(
                                &tile.pixels[tile_row_start..tile_row_start + len],
                            );
                        }
                    }
                }
            });

        pixels
    }

    /// Extract a rectangular region into a flat array (for partial update / dirty region).
    pub fn extract_region(&self, x0: u32, y0: u32, w: u32, h: u32) -> Vec<u8> {
        let Some(len) = (w as u64)
            .checked_mul(h as u64)
            .and_then(|n| n.checked_mul(4))
            .and_then(|n| usize::try_from(n).ok())
        else {
            return Vec::new();
        };
        let mut out = vec![0u8; len];
        self.flatten_tiles_region_into(x0, y0, w, h, &mut out);
        out
    }

    pub fn flatten_tiles_region_into(&self, x0: u32, y0: u32, w: u32, h: u32, out: &mut [u8]) {
        if w == 0 || h == 0 {
            return;
        }
        out.fill(0);

        for row in 0..h {
            let py = y0 + row;
            if py >= self.height {
                break;
            }
            let ty = py / TILE_SIZE;
            let ty_rem = py % TILE_SIZE;

            let mut px = x0;
            while px < x0 + w {
                if px >= self.width {
                    break;
                }
                let tx = px / TILE_SIZE;
                let tx_rem = px % TILE_SIZE;

                let tile_w = TILE_SIZE - tx_rem;
                let copy_w = tile_w.min(x0 + w - px).min(self.width - px);

                let pos = TilePos {
                    x: tx as i32,
                    y: ty as i32,
                };
                if let Some(tile) = self.tiles.get(&pos) {
                    let src_i = ((ty_rem * TILE_SIZE + tx_rem) * 4) as usize;
                    let dst_i = ((row * w + (px - x0)) * 4) as usize;
                    let len = (copy_w * 4) as usize;

                    if src_i + len <= tile.pixels.len() && dst_i + len <= out.len() {
                        out[dst_i..dst_i + len].copy_from_slice(&tile.pixels[src_i..src_i + len]);
                    }
                }
                px += copy_w;
            }
        }
    }

    /// 16-bit-master version of [`flatten_tiles_region_into`]: reads each tile's
    /// 16-bit master into `out` (RGBA16), falling back to the 8-bit mirror
    /// up-converted as `v*257` for any tile without a master. Lets crop and other
    /// blit-based rebuilds copy a region at full precision.
    pub fn flatten16_region_into(&self, x0: u32, y0: u32, w: u32, h: u32, out: &mut [u16]) {
        if w == 0 || h == 0 {
            return;
        }
        out.fill(0);

        for row in 0..h {
            let py = y0 + row;
            if py >= self.height {
                break;
            }
            let ty = py / TILE_SIZE;
            let ty_rem = py % TILE_SIZE;

            let mut px = x0;
            while px < x0 + w {
                if px >= self.width {
                    break;
                }
                let tx = px / TILE_SIZE;
                let tx_rem = px % TILE_SIZE;

                let tile_w = TILE_SIZE - tx_rem;
                let copy_w = tile_w.min(x0 + w - px).min(self.width - px);

                let pos = TilePos {
                    x: tx as i32,
                    y: ty as i32,
                };
                if let Some(tile) = self.tiles.get(&pos) {
                    let src_i = ((ty_rem * TILE_SIZE + tx_rem) * 4) as usize;
                    let dst_i = ((row * w + (px - x0)) * 4) as usize;
                    let len = (copy_w * 4) as usize;

                    if dst_i + len <= out.len() {
                        if let Some(p16) = &tile.pixels16 {
                            if src_i + len <= p16.len() {
                                out[dst_i..dst_i + len].copy_from_slice(&p16[src_i..src_i + len]);
                            }
                        } else if src_i + len <= tile.pixels.len() {
                            for k in 0..len {
                                out[dst_i + k] = tile.pixels[src_i + k] as u16 * 257;
                            }
                        }
                    }
                }
                px += copy_w;
            }
        }
    }

    /// Copy a `w×h` rectangle from `src` (at `src_x,src_y`) into `self` (at
    /// `dst_x,dst_y`) in 256-px chunks, never allocating a canvas-sized buffer.
    /// This is the tile-native replacement for the `extract_region → flat →
    /// from_rgba` idiom used by crop/resize, so those ops work on Viewport-
    /// Streaming (>25M px) canvases. `self` and `src` must be different maps.
    ///
    /// CMYK: when `src` carries ink planes they are copied in a second pass
    /// (the mirror pass's `get_tile_mut` drops any partial ink, so ink must be
    /// written only after every mirror chunk touching a tile is done).
    pub fn blit_region_from(
        &mut self,
        src: &TileMap,
        src_x: u32,
        src_y: u32,
        dst_x: u32,
        dst_y: u32,
        w: u32,
        h: u32,
    ) {
        if w == 0 || h == 0 {
            return;
        }
        let chunk = TILE_SIZE;
        // When the source carries a full 16-bit master, copy at 16 bits so the
        // rebuild (crop, flip, resize-to-same-size) keeps precision;
        // write_region16 refreshes each destination tile's 8-bit mirror too.
        // Otherwise copy the 8-bit mirror as before.
        if src.has_hdr() {
            let mut buf16 = vec![0u16; (chunk * chunk * 4) as usize];
            let mut cy = 0;
            while cy < h {
                let ch = chunk.min(h - cy);
                let mut cx = 0;
                while cx < w {
                    let cw = chunk.min(w - cx);
                    let needed = (cw * ch * 4) as usize;
                    src.flatten16_region_into(src_x + cx, src_y + cy, cw, ch, &mut buf16[..needed]);
                    self.write_region16(dst_x + cx, dst_y + cy, cw, ch, &buf16[..needed]);
                    cx += cw;
                }
                cy += ch;
            }
        } else {
            let mut buf = vec![0u8; (chunk * chunk * 4) as usize];
            let mut cy = 0;
            while cy < h {
                let ch = chunk.min(h - cy);
                let mut cx = 0;
                while cx < w {
                    let cw = chunk.min(w - cx);
                    let needed = (cw * ch * 4) as usize;
                    src.flatten_tiles_region_into(
                        src_x + cx,
                        src_y + cy,
                        cw,
                        ch,
                        &mut buf[..needed],
                    );
                    self.write_region(dst_x + cx, dst_y + cy, cw, ch, &buf[..needed]);
                    cx += cw;
                }
                cy += ch;
            }
        }

        if src.has_any_ink() {
            let mut buf = vec![0u8; (chunk * chunk * 4) as usize];
            let mut cy = 0;
            while cy < h {
                let ch = chunk.min(h - cy);
                let mut cx = 0;
                while cx < w {
                    let cw = chunk.min(w - cx);
                    let needed = (cw * ch * 4) as usize;
                    src.extract_ink_region_into(src_x + cx, src_y + cy, cw, ch, &mut buf[..needed]);
                    self.write_ink_region(dst_x + cx, dst_y + cy, cw, ch, &buf[..needed]);
                    cx += cw;
                }
                cy += ch;
            }
        }
    }

    pub fn rotate_90_cw(&self) -> Self {
        let new_w = self.height;
        let new_h = self.width;
        let mut new_map = Self::new(new_w, new_h);
        let cols = (new_w + TILE_SIZE - 1) / TILE_SIZE;
        let rows = (new_h + TILE_SIZE - 1) / TILE_SIZE;
        let rev = NEXT_TILE_REV.fetch_add(1, Ordering::Relaxed);
        // Permute the 16-bit master too when the source has one, so a lossless
        // rotate/flip of a 16-bit layer stays 16-bit instead of quantizing.
        let src_hdr = self.has_hdr();

        let tiles: Vec<(TilePos, Arc<Tile>)> = (0..rows)
            .into_par_iter()
            .flat_map(move |cy| {
                let mut local = Vec::new();
                for cx in 0..cols {
                    let mut tile_pixels = vec![0u8; TILE_BYTES];
                    let mut tile_pixels16 = src_hdr.then(|| vec![0u16; TILE_BYTES]);
                    let mut has_data = false;
                    for ty in 0..TILE_SIZE {
                        let ny = cy * TILE_SIZE + ty;
                        if ny >= new_h {
                            continue;
                        }
                        for tx in 0..TILE_SIZE {
                            let nx = cx * TILE_SIZE + tx;
                            if nx >= new_w {
                                continue;
                            }
                            let ox = ny;
                            let oy = self.height.saturating_sub(1).saturating_sub(nx);
                            let (r, g, b, a) = self.get_pixel(ox, oy);
                            if a > 0 {
                                has_data = true;
                                let i = ((ty * TILE_SIZE + tx) * 4) as usize;
                                tile_pixels[i] = r;
                                tile_pixels[i + 1] = g;
                                tile_pixels[i + 2] = b;
                                tile_pixels[i + 3] = a;
                                if let Some(p16) = tile_pixels16.as_mut() {
                                    let (r16, g16, b16, a16) = self.get_pixel16(ox, oy);
                                    p16[i] = r16;
                                    p16[i + 1] = g16;
                                    p16[i + 2] = b16;
                                    p16[i + 3] = a16;
                                }
                            }
                        }
                    }
                    if has_data {
                        local.push((
                            TilePos {
                                x: cx as i32,
                                y: cy as i32,
                            },
                            Arc::new(Tile {
                                pixels: tile_pixels,
                                pixels16: tile_pixels16,
                                // Rotate/flip do not carry ink; CMYK docs gate
                                // these ops at the UI (v1).
                                ink: None,
                                revision: rev,
                            }),
                        ));
                    }
                }
                local
            })
            .collect();
        for (p, t) in tiles {
            new_map.tiles.insert(p, t);
        }
        new_map
    }

    pub fn rotate_90_ccw(&self) -> Self {
        let new_w = self.height;
        let new_h = self.width;
        let mut new_map = Self::new(new_w, new_h);
        let cols = (new_w + TILE_SIZE - 1) / TILE_SIZE;
        let rows = (new_h + TILE_SIZE - 1) / TILE_SIZE;
        let rev = NEXT_TILE_REV.fetch_add(1, Ordering::Relaxed);
        // Permute the 16-bit master too when the source has one, so a lossless
        // rotate/flip of a 16-bit layer stays 16-bit instead of quantizing.
        let src_hdr = self.has_hdr();

        let tiles: Vec<(TilePos, Arc<Tile>)> = (0..rows)
            .into_par_iter()
            .flat_map(move |cy| {
                let mut local = Vec::new();
                for cx in 0..cols {
                    let mut tile_pixels = vec![0u8; TILE_BYTES];
                    let mut tile_pixels16 = src_hdr.then(|| vec![0u16; TILE_BYTES]);
                    let mut has_data = false;
                    for ty in 0..TILE_SIZE {
                        let ny = cy * TILE_SIZE + ty;
                        if ny >= new_h {
                            continue;
                        }
                        for tx in 0..TILE_SIZE {
                            let nx = cx * TILE_SIZE + tx;
                            if nx >= new_w {
                                continue;
                            }
                            let ox = self.width.saturating_sub(1).saturating_sub(ny);
                            let oy = nx;
                            let (r, g, b, a) = self.get_pixel(ox, oy);
                            if a > 0 {
                                has_data = true;
                                let i = ((ty * TILE_SIZE + tx) * 4) as usize;
                                tile_pixels[i] = r;
                                tile_pixels[i + 1] = g;
                                tile_pixels[i + 2] = b;
                                tile_pixels[i + 3] = a;
                                if let Some(p16) = tile_pixels16.as_mut() {
                                    let (r16, g16, b16, a16) = self.get_pixel16(ox, oy);
                                    p16[i] = r16;
                                    p16[i + 1] = g16;
                                    p16[i + 2] = b16;
                                    p16[i + 3] = a16;
                                }
                            }
                        }
                    }
                    if has_data {
                        local.push((
                            TilePos {
                                x: cx as i32,
                                y: cy as i32,
                            },
                            Arc::new(Tile {
                                pixels: tile_pixels,
                                pixels16: tile_pixels16,
                                // Rotate/flip do not carry ink; CMYK docs gate
                                // these ops at the UI (v1).
                                ink: None,
                                revision: rev,
                            }),
                        ));
                    }
                }
                local
            })
            .collect();
        for (p, t) in tiles {
            new_map.tiles.insert(p, t);
        }
        new_map
    }

    pub fn flip_h(&self) -> Self {
        let mut new_map = Self::new(self.width, self.height);
        let cols = (self.width + TILE_SIZE - 1) / TILE_SIZE;
        let rows = (self.height + TILE_SIZE - 1) / TILE_SIZE;
        let rev = NEXT_TILE_REV.fetch_add(1, Ordering::Relaxed);
        // Permute the 16-bit master too when the source has one, so a lossless
        // rotate/flip of a 16-bit layer stays 16-bit instead of quantizing.
        let src_hdr = self.has_hdr();

        let tiles: Vec<(TilePos, Arc<Tile>)> = (0..rows)
            .into_par_iter()
            .flat_map(move |cy| {
                let mut local = Vec::new();
                for cx in 0..cols {
                    let mut tile_pixels = vec![0u8; TILE_BYTES];
                    let mut tile_pixels16 = src_hdr.then(|| vec![0u16; TILE_BYTES]);
                    let mut has_data = false;
                    for ty in 0..TILE_SIZE {
                        let ny = cy * TILE_SIZE + ty;
                        if ny >= self.height {
                            continue;
                        }
                        for tx in 0..TILE_SIZE {
                            let nx = cx * TILE_SIZE + tx;
                            if nx >= self.width {
                                continue;
                            }
                            let ox = self.width.saturating_sub(1).saturating_sub(nx);
                            let oy = ny;
                            let (r, g, b, a) = self.get_pixel(ox, oy);
                            if a > 0 {
                                has_data = true;
                                let i = ((ty * TILE_SIZE + tx) * 4) as usize;
                                tile_pixels[i] = r;
                                tile_pixels[i + 1] = g;
                                tile_pixels[i + 2] = b;
                                tile_pixels[i + 3] = a;
                                if let Some(p16) = tile_pixels16.as_mut() {
                                    let (r16, g16, b16, a16) = self.get_pixel16(ox, oy);
                                    p16[i] = r16;
                                    p16[i + 1] = g16;
                                    p16[i + 2] = b16;
                                    p16[i + 3] = a16;
                                }
                            }
                        }
                    }
                    if has_data {
                        local.push((
                            TilePos {
                                x: cx as i32,
                                y: cy as i32,
                            },
                            Arc::new(Tile {
                                pixels: tile_pixels,
                                pixels16: tile_pixels16,
                                // Rotate/flip do not carry ink; CMYK docs gate
                                // these ops at the UI (v1).
                                ink: None,
                                revision: rev,
                            }),
                        ));
                    }
                }
                local
            })
            .collect();
        for (p, t) in tiles {
            new_map.tiles.insert(p, t);
        }
        new_map
    }

    /// Nearest-neighbor interpolate a pixel at fractional position (x, y) in layer-local coords.
    /// Returns (0,0,0,0) for out-of-bounds positions.
    pub fn sample_nearest(&self, x: f32, y: f32) -> (u8, u8, u8, u8) {
        let px = x.round() as i32;
        let py = y.round() as i32;
        if px < 0 || py < 0 || px >= self.width as i32 || py >= self.height as i32 {
            return (0, 0, 0, 0);
        }
        self.get_pixel(px as u32, py as u32)
    }

    /// Bilinear-interpolate a pixel at fractional position (x, y) in layer-local coords.
    /// Uses pre-multiplied alpha for correct blending across transparent boundaries.
    /// Returns (0,0,0,0) for out-of-bounds positions.
    pub fn sample_bilinear(&self, x: f32, y: f32) -> (u8, u8, u8, u8) {
        let x0 = x.floor() as i32;
        let y0 = y.floor() as i32;
        let fx = x - x0 as f32;
        let fy = y - y0 as f32;

        let samp = |px: i32, py: i32| -> [f32; 4] {
            if px < 0 || py < 0 || px >= self.width as i32 || py >= self.height as i32 {
                return [0.0; 4];
            }
            let (r, g, b, a) = self.get_pixel(px as u32, py as u32);
            let af = a as f32 / 255.0;
            [
                r as f32 * af / 255.0,
                g as f32 * af / 255.0,
                b as f32 * af / 255.0,
                af,
            ]
        };

        let (c00, c10, c01, c11) = if x0 >= 0
            && y0 >= 0
            && ((x0 + 1) as u32) < self.width
            && ((y0 + 1) as u32) < self.height
        {
            let xu = x0 as u32;
            let yu = y0 as u32;
            let lx = xu % TILE_SIZE;
            let ly = yu % TILE_SIZE;
            if lx + 1 < TILE_SIZE && ly + 1 < TILE_SIZE {
                let pos = TilePos {
                    x: (xu / TILE_SIZE) as i32,
                    y: (yu / TILE_SIZE) as i32,
                };
                match self.tiles.get(&pos) {
                    Some(tile) => {
                        let p = &tile.pixels;
                        let row = (TILE_SIZE * 4) as usize;
                        let base = ((ly * TILE_SIZE + lx) * 4) as usize;
                        let pm = |i: usize| -> [f32; 4] {
                            let af = p[i + 3] as f32 / 255.0;
                            [
                                p[i] as f32 * af / 255.0,
                                p[i + 1] as f32 * af / 255.0,
                                p[i + 2] as f32 * af / 255.0,
                                af,
                            ]
                        };
                        (pm(base), pm(base + 4), pm(base + row), pm(base + row + 4))
                    }
                    None => ([0.0; 4], [0.0; 4], [0.0; 4], [0.0; 4]),
                }
            } else {
                (
                    samp(x0, y0),
                    samp(x0 + 1, y0),
                    samp(x0, y0 + 1),
                    samp(x0 + 1, y0 + 1),
                )
            }
        } else {
            (
                samp(x0, y0),
                samp(x0 + 1, y0),
                samp(x0, y0 + 1),
                samp(x0 + 1, y0 + 1),
            )
        };

        let lerp = |a: f32, b: f32, t: f32| -> f32 { a * (1.0 - t) + b * t };
        let a = lerp(lerp(c00[3], c10[3], fx), lerp(c01[3], c11[3], fx), fy);
        if a < f32::EPSILON {
            return (0, 0, 0, 0);
        }
        let r = lerp(lerp(c00[0], c10[0], fx), lerp(c01[0], c11[0], fx), fy) / a;
        let g = lerp(lerp(c00[1], c10[1], fx), lerp(c01[1], c11[1], fx), fy) / a;
        let b = lerp(lerp(c00[2], c10[2], fx), lerp(c01[2], c11[2], fx), fy) / a;

        fn to_u8(v: f32) -> u8 {
            (v * 255.0).round().clamp(0.0, 255.0) as u8
        }
        (to_u8(r), to_u8(g), to_u8(b), to_u8(a))
    }

    /// 16-bit counterpart of [`sample_bilinear`]: same premultiplied-alpha bilinear
    /// math, reading the 16-bit master (`get_pixel16` falls back to the up-converted
    /// mirror) so resampling a 16-bit layer keeps precision. Out-of-bounds →
    /// `(0,0,0,0)`.
    pub fn sample_bilinear16(&self, x: f32, y: f32) -> (u16, u16, u16, u16) {
        let x0 = x.floor() as i32;
        let y0 = y.floor() as i32;
        let fx = x - x0 as f32;
        let fy = y - y0 as f32;

        let samp = |px: i32, py: i32| -> [f32; 4] {
            if px < 0 || py < 0 || px >= self.width as i32 || py >= self.height as i32 {
                return [0.0; 4];
            }
            let (r, g, b, a) = self.get_pixel16(px as u32, py as u32);
            let af = a as f32 / 65535.0;
            [
                r as f32 * af / 65535.0,
                g as f32 * af / 65535.0,
                b as f32 * af / 65535.0,
                af,
            ]
        };

        let c00 = samp(x0, y0);
        let c10 = samp(x0 + 1, y0);
        let c01 = samp(x0, y0 + 1);
        let c11 = samp(x0 + 1, y0 + 1);

        let lerp = |a: f32, b: f32, t: f32| -> f32 { a * (1.0 - t) + b * t };
        let a = lerp(lerp(c00[3], c10[3], fx), lerp(c01[3], c11[3], fx), fy);
        if a < f32::EPSILON {
            return (0, 0, 0, 0);
        }
        let r = lerp(lerp(c00[0], c10[0], fx), lerp(c01[0], c11[0], fx), fy) / a;
        let g = lerp(lerp(c00[1], c10[1], fx), lerp(c01[1], c11[1], fx), fy) / a;
        let b = lerp(lerp(c00[2], c10[2], fx), lerp(c01[2], c11[2], fx), fy) / a;

        fn to_u16(v: f32) -> u16 {
            (v * 65535.0).round().clamp(0.0, 65535.0) as u16
        }
        (to_u16(r), to_u16(g), to_u16(b), to_u16(a))
    }

    /// Returns the tight bounding box of all non-transparent pixels in layer-local coords.
    /// Result is (min_x, min_y, max_x_exclusive, max_y_exclusive), all in layer pixels.
    /// Returns `None` if the TileMap is entirely transparent / empty.
    pub fn content_bounds(&self) -> Option<(i32, i32, i32, i32)> {
        if self.tiles.is_empty() {
            return None;
        }
        let mut min_x = i32::MAX;
        let mut min_y = i32::MAX;
        let mut max_x = i32::MIN;
        let mut max_y = i32::MIN;

        for (pos, tile) in &self.tiles {
            let tile_px = pos.x as i32 * TILE_SIZE as i32;
            let tile_py = pos.y as i32 * TILE_SIZE as i32;
            let data = &tile.pixels;

            let has_alpha = data.chunks(4).any(|px| px[3] > 0);
            if !has_alpha {
                continue;
            }

            for py in 0..TILE_SIZE as i32 {
                let row_base = (py as usize * TILE_SIZE as usize) * 4;
                let cy = tile_py + py;
                if cy < 0 || cy >= self.height as i32 {
                    continue;
                }
                for px in 0..TILE_SIZE as i32 {
                    let cx = tile_px + px;
                    if cx < 0 || cx >= self.width as i32 {
                        continue;
                    }
                    let a = data[row_base + px as usize * 4 + 3];
                    if a > 0 {
                        if cx < min_x {
                            min_x = cx;
                        }
                        if cy < min_y {
                            min_y = cy;
                        }
                        if cx + 1 > max_x {
                            max_x = cx + 1;
                        }
                        if cy + 1 > max_y {
                            max_y = cy + 1;
                        }
                    }
                }
            }
        }

        if min_x == i32::MAX {
            None
        } else {
            Some((min_x, min_y, max_x, max_y))
        }
    }

    pub fn flip_v(&self) -> Self {
        let mut new_map = Self::new(self.width, self.height);
        let cols = (self.width + TILE_SIZE - 1) / TILE_SIZE;
        let rows = (self.height + TILE_SIZE - 1) / TILE_SIZE;
        let rev = NEXT_TILE_REV.fetch_add(1, Ordering::Relaxed);
        // Permute the 16-bit master too when the source has one, so a lossless
        // rotate/flip of a 16-bit layer stays 16-bit instead of quantizing.
        let src_hdr = self.has_hdr();

        let tiles: Vec<(TilePos, Arc<Tile>)> = (0..rows)
            .into_par_iter()
            .flat_map(move |cy| {
                let mut local = Vec::new();
                for cx in 0..cols {
                    let mut tile_pixels = vec![0u8; TILE_BYTES];
                    let mut tile_pixels16 = src_hdr.then(|| vec![0u16; TILE_BYTES]);
                    let mut has_data = false;
                    for ty in 0..TILE_SIZE {
                        let ny = cy * TILE_SIZE + ty;
                        if ny >= self.height {
                            continue;
                        }
                        for tx in 0..TILE_SIZE {
                            let nx = cx * TILE_SIZE + tx;
                            if nx >= self.width {
                                continue;
                            }
                            let ox = nx;
                            let oy = self.height.saturating_sub(1).saturating_sub(ny);
                            let (r, g, b, a) = self.get_pixel(ox, oy);
                            if a > 0 {
                                has_data = true;
                                let i = ((ty * TILE_SIZE + tx) * 4) as usize;
                                tile_pixels[i] = r;
                                tile_pixels[i + 1] = g;
                                tile_pixels[i + 2] = b;
                                tile_pixels[i + 3] = a;
                                if let Some(p16) = tile_pixels16.as_mut() {
                                    let (r16, g16, b16, a16) = self.get_pixel16(ox, oy);
                                    p16[i] = r16;
                                    p16[i + 1] = g16;
                                    p16[i + 2] = b16;
                                    p16[i + 3] = a16;
                                }
                            }
                        }
                    }
                    if has_data {
                        local.push((
                            TilePos {
                                x: cx as i32,
                                y: cy as i32,
                            },
                            Arc::new(Tile {
                                pixels: tile_pixels,
                                pixels16: tile_pixels16,
                                // Rotate/flip do not carry ink; CMYK docs gate
                                // these ops at the UI (v1).
                                ink: None,
                                revision: rev,
                            }),
                        ));
                    }
                }
                local
            })
            .collect();
        for (p, t) in tiles {
            new_map.tiles.insert(p, t);
        }
        new_map
    }
}

// ── Channels-panel write gate ───────────────────────────────────────────────
// Tool dab loops call these when a subset of colour channels is
// write-enabled. Channel edits behave like painting on a grayscale plate:
// only the enabled channels move, and alpha never changes.

/// Rec.709 luma of a colour — the gray value that colour paints into a
/// single-channel plate.
#[inline]
pub fn luma_u8(r: u8, g: u8, b: u8) -> u8 {
    (r as f32 * 0.2126 + g as f32 * 0.7152 + b as f32 * 0.0722)
        .round()
        .clamp(0.0, 255.0) as u8
}

/// Blend `src` (normalised RGB) into the write-enabled colour channels of a
/// pixel by `cov`, leaving the other channels and alpha untouched. Colour-
/// source tools pass `[luma; 3]`; the clone tool passes the source pixel so
/// each plate copies its own channel.
#[inline]
pub fn blend_masked(px: &mut [u8], src: [f32; 3], cov: f32, wm: [bool; 4]) {
    let cov = cov.clamp(0.0, 1.0);
    for c in 0..3 {
        if wm[c] {
            let d = px[c] as f32 / 255.0;
            px[c] = ((d + (src[c] - d) * cov) * 255.0).round() as u8;
        }
    }
}

/// Write gate for pixel-transform tools (smudge/dodge/burn): keep only the
/// write-enabled colour channels of the computed result, restoring the rest
/// and alpha from the pre-edit pixel.
#[inline]
pub fn apply_write_mask(before: [u8; 4], px: &mut [u8; 4], wm: [bool; 4]) {
    for c in 0..3 {
        if !wm[c] {
            px[c] = before[c];
        }
    }
    px[3] = before[3];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotate_and_flip_preserve_16bit_master() {
        // Lossless permutations of a 16-bit layer must keep the master; before
        // this each rebuilt tiles at 8 bits and dropped it. Uniform sub-8-bit
        // values (300 is not a `v*257`) prove precision survived; the geometry
        // itself is covered by the existing 8-bit rotate/flip tests.
        let (w, h) = (20u32, 12u32);
        let mut px16 = vec![0u16; (w * h * 4) as usize];
        for p in 0..(w * h) as usize {
            px16[p * 4] = 300;
            px16[p * 4 + 1] = 40000;
            px16[p * 4 + 2] = 12345;
            px16[p * 4 + 3] = 65535;
        }
        let src = TileMap::from_rgba16(&px16, w, h);
        assert!(src.has_hdr());
        let want = (300u16, 40000u16, 12345u16, 65535u16);

        let cw = src.rotate_90_cw();
        assert_eq!((cw.width, cw.height), (h, w));
        assert!(cw.has_hdr(), "rotate_90_cw dropped the master");
        assert_eq!(cw.get_pixel16(3, 3), want);

        let ccw = src.rotate_90_ccw();
        assert!(ccw.has_hdr(), "rotate_90_ccw dropped the master");
        assert_eq!(ccw.get_pixel16(3, 3), want);

        let fh = src.flip_h();
        assert!(fh.has_hdr(), "flip_h dropped the master");
        assert_eq!(fh.get_pixel16(3, 3), want);

        let fv = src.flip_v();
        assert!(fv.has_hdr(), "flip_v dropped the master");
        assert_eq!(fv.get_pixel16(3, 3), want);
    }

    #[test]
    fn blend_masked_touches_only_enabled_channels() {
        let mut px = [10u8, 20, 30, 40];
        blend_masked(&mut px, [1.0; 3], 1.0, [true, false, false, true]);
        assert_eq!(px, [255, 20, 30, 40], "only R moves, alpha untouched");

        let mut px = [0u8, 0, 0, 128];
        blend_masked(&mut px, [1.0; 3], 0.5, [false, true, true, true]);
        assert_eq!(px, [0, 128, 128, 128], "half coverage lerps G/B");
    }

    #[test]
    fn apply_write_mask_restores_unselected_and_alpha() {
        let before = [10u8, 20, 30, 40];
        let mut px = [100u8, 110, 120, 130];
        apply_write_mask(before, &mut px, [false, true, false, true]);
        assert_eq!(px, [10, 110, 30, 40]);
    }

    #[test]
    fn luma_matches_rec709() {
        assert_eq!(luma_u8(255, 255, 255), 255);
        assert_eq!(luma_u8(0, 0, 0), 0);
        assert_eq!(luma_u8(255, 0, 0), 54);
    }

    #[test]
    fn downsample_half_halves_dims_and_preserves_solid_color() {
        let map = TileMap::new_solid(600, 400, 12, 34, 56, 255);
        let ds = map.downsample_half();
        assert_eq!((ds.width, ds.height), (300, 200), "ceil(w/2), ceil(h/2)");
        // A uniform field stays that colour under a box filter.
        for &(x, y) in &[(0u32, 0u32), (150, 100), (299, 199)] {
            assert_eq!(ds.get_pixel(x, y), (12, 34, 56, 255));
        }
    }

    #[test]
    fn downsample_half_averages_2x2_block() {
        // A 2×2 block of black/white/black/white averages to mid-grey (128).
        let mut map = TileMap::new(2, 2);
        map.set_pixel(0, 0, 255, 255, 255, 255);
        map.set_pixel(1, 0, 0, 0, 0, 255);
        map.set_pixel(0, 1, 0, 0, 0, 255);
        map.set_pixel(1, 1, 255, 255, 255, 255);
        let ds = map.downsample_half();
        assert_eq!((ds.width, ds.height), (1, 1));
        // (255+0+0+255)/4 = 127 (round-half-up on the premult sum lands at 128).
        let (r, g, b, a) = ds.get_pixel(0, 0);
        assert!((127..=128).contains(&r), "grey ~128, got {r}");
        assert_eq!((g, b, a), (r, r, 255));
    }

    #[test]
    fn downsample_half_odd_dims_ceil() {
        let map = TileMap::new_solid(3, 5, 9, 9, 9, 255);
        let ds = map.downsample_half();
        assert_eq!((ds.width, ds.height), (2, 3), "ceil of odd dims");
    }

    #[test]
    fn downsample_half_premultiplied_edge() {
        // One opaque red + three transparent samples: colour must stay pure red
        // (premultiplied average) while alpha drops to ~1/4.
        let mut map = TileMap::new(2, 2);
        map.set_pixel(0, 0, 255, 0, 0, 255);
        // (1,0),(0,1),(1,1) left transparent (0,0,0,0)
        let ds = map.downsample_half();
        let (r, g, b, a) = ds.get_pixel(0, 0);
        assert_eq!((r, g, b), (255, 0, 0), "colour not polluted by transparent");
        assert!((63..=64).contains(&a), "alpha ~64, got {a}");
    }

    #[test]
    fn downsample_half_drops_fully_transparent_tiles() {
        // An empty map yields an empty (sparse) proxy, never phantom tiles.
        let map = TileMap::new(512, 512);
        let ds = map.downsample_half();
        assert!(ds.tiles.is_empty());
    }

    // ── CMYK ink planes ─────────────────────────────────────────────────────

    use crate::core::cms::{naive_cmyk_to_rgb, naive_rgb_to_cmyk, CmykConverter};

    /// Every tile with an ink plane must mirror it exactly: pixels' RGB ==
    /// converter projection of the ink, for every VISIBLE pixel (alpha > 0 —
    /// a transparent pixel's RGB is meaningless, and zero ink there projects
    /// to paper-white which the mirror never stores). This is THE CMYK-document
    /// invariant; call it after any ink-touching operation.
    fn assert_ink_mirror_consistent(map: &TileMap, conv: &CmykConverter) {
        for (pos, tile) in &map.tiles {
            let Some(plane) = tile.ink.as_ref() else {
                continue;
            };
            for p in 0..TILE_PIXELS {
                let i = p * 4;
                if tile.pixels[i + 3] == 0 {
                    continue;
                }
                let ink = [plane[i], plane[i + 1], plane[i + 2], plane[i + 3]];
                let rgb = conv.cmyk_to_rgb_one(ink);
                assert_eq!(
                    [tile.pixels[i], tile.pixels[i + 1], tile.pixels[i + 2]],
                    rgb,
                    "ink/mirror desync at tile {pos:?} px {p}"
                );
            }
        }
    }

    /// Paint one ink pixel through the canonical write path (mutate ink, then
    /// re-project the mirror) so tests exercise what tools will do.
    fn paint_ink_px(map: &mut TileMap, x: u32, y: u32, ink: [u8; 4], alpha: u8) {
        let pos = TilePos::from_pixel(x, y);
        let t = map.get_tile_mut_ink(pos);
        let i = (((y % TILE_SIZE) * TILE_SIZE + (x % TILE_SIZE)) * 4) as usize;
        t.ink.as_mut().unwrap()[i..i + 4].copy_from_slice(&ink);
        t.pixels[i + 3] = alpha;
        map.refresh_mirror_from_ink(x, y, 1, 1, &CmykConverter::Naive);
    }

    #[test]
    fn ink_counts_toward_byte_size() {
        let mut t = Tile::new_empty();
        let base = t.byte_size();
        t.ink = Some(vec![0u8; TILE_BYTES]);
        assert_eq!(t.byte_size(), base + TILE_BYTES);
    }

    #[test]
    fn get_tile_mut_drops_ink_but_ink_getter_keeps_it() {
        let mut map = TileMap::new(64, 64);
        let pos = TilePos { x: 0, y: 0 };
        map.get_tile_mut_ink(pos).ink.as_mut().unwrap()[0] = 200;
        assert_eq!(map.get_tile_mut_ink(pos).ink.as_ref().unwrap()[0], 200);
        // An RGB-path mutation invalidates the plane (fail-loud, not desync).
        assert!(map.get_tile_mut(pos).ink.is_none());
    }

    #[test]
    fn ink_region_roundtrips_across_tiles() {
        let mut map = TileMap::new(600, 600);
        // 3×2 px straddling the tile border at x=256.
        let src = [
            10u8, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120, //
            130, 140, 150, 160, 170, 180, 190, 200, 210, 220, 230, 240,
        ];
        map.write_ink_region(255, 100, 3, 2, &src);
        let mut out = vec![0u8; src.len()];
        map.extract_ink_region_into(255, 100, 3, 2, &mut out);
        assert_eq!(out, src);
        // Outside the written rect reads as zero ink.
        let mut none = vec![9u8; 4];
        map.extract_ink_region_into(0, 0, 1, 1, &mut none);
        assert_eq!(none, [0, 0, 0, 0]);
    }

    #[test]
    fn refresh_mirror_projects_ink_and_keeps_alpha() {
        let mut map = TileMap::new(64, 64);
        {
            let t = map.get_tile_mut_ink(TilePos { x: 0, y: 0 });
            let i = ((3 * TILE_SIZE + 5) * 4) as usize;
            t.ink.as_mut().unwrap()[i..i + 4].copy_from_slice(&[0, 0, 0, 127]);
            t.pixels[i + 3] = 200; // alpha set independently of ink
        }
        map.refresh_mirror_from_ink(0, 0, 64, 64, &CmykConverter::Naive);
        let (r, g, b, a) = map.get_pixel(5, 3);
        assert_eq!([r, g, b], naive_cmyk_to_rgb([0, 0, 0, 127]));
        assert_eq!(a, 200, "alpha is not part of the ink projection");
        assert_ink_mirror_consistent(&map, &CmykConverter::Naive);
    }

    #[test]
    fn translate_carries_ink_aligned_and_unaligned() {
        let ink = naive_rgb_to_cmyk([200, 30, 90]);
        // Aligned (multiple of TILE_SIZE): tiles are re-keyed wholesale.
        let mut map = TileMap::new(1024, 1024);
        paint_ink_px(&mut map, 10, 10, ink, 255);
        map.translate(TILE_SIZE as i32, 0);
        let mut got = [0u8; 4];
        map.extract_ink_region_into(10 + TILE_SIZE, 10, 1, 1, &mut got);
        assert_eq!(got, ink, "aligned translate must carry the ink plane");

        // Unaligned: per-pixel rebuild must copy ink alongside RGBA.
        let mut map = TileMap::new(1024, 1024);
        paint_ink_px(&mut map, 10, 10, ink, 255);
        map.translate(3, 5);
        let mut got = [0u8; 4];
        map.extract_ink_region_into(13, 15, 1, 1, &mut got);
        assert_eq!(got, ink, "unaligned translate must carry the ink plane");
        assert_ink_mirror_consistent(&map, &CmykConverter::Naive);
    }

    #[test]
    fn blit_region_carries_ink_with_mirror() {
        // Crop path: rebuild a smaller map from a region of the source. The ink
        // pass runs after all mirror chunks, so tiles straddling chunk borders
        // keep a complete plane.
        let conv = CmykConverter::Naive;
        let mut src = TileMap::new(700, 700);
        for (x, y, rgb) in [
            (100u32, 100u32, [255u8, 0, 0]),
            (400, 300, [0, 128, 255]),
            (650, 650, [1, 2, 3]),
        ] {
            paint_ink_px(&mut src, x, y, naive_rgb_to_cmyk(rgb), 255);
        }
        let mut dst = TileMap::new(620, 620);
        dst.blit_region_from(&src, 80, 90, 0, 0, 620, 610);

        for (x, y, rgb) in [(100u32, 100u32, [255u8, 0, 0]), (400, 300, [0, 128, 255])] {
            let mut got = [0u8; 4];
            dst.extract_ink_region_into(x - 80, y - 90, 1, 1, &mut got);
            assert_eq!(got, naive_rgb_to_cmyk(rgb), "ink lost in blit at {x},{y}");
            let (r, g, b, _) = dst.get_pixel(x - 80, y - 90);
            assert_eq!([r, g, b], naive_cmyk_to_rgb(naive_rgb_to_cmyk(rgb)));
        }
        assert_ink_mirror_consistent(&dst, &conv);
    }

    #[test]
    fn dither16_spreads_half_level_and_preserves_mean() {
        // A 16-bit value that maps to a "half" 8-bit level (~100.5) must dither into
        // both 100 and 101 across an 8×8 block (no flat band), with the block mean
        // landing within half a level of the true value — the extra bits recovered
        // perceptually instead of posterized. Truncation (`>> 8`) would give one flat
        // value and lose the mean, which is the RAW-sky banding we're fixing.
        let target = 100.5f32;
        let v16 = (target / 255.0 * 65535.0).round() as u16;
        let mut sum = 0u32;
        let (mut lo, mut hi) = (255u8, 0u8);
        for y in 0..8u32 {
            for x in 0..8u32 {
                let q = dither16_to_u8(v16, x, y, 0);
                sum += q as u32;
                lo = lo.min(q);
                hi = hi.max(q);
            }
        }
        assert!(hi > lo, "dither must spread the half-level, not posterize");
        assert!(hi - lo <= 1, "spread should stay within one 8-bit level");
        let mean = sum as f32 / 64.0;
        let truth = v16 as f32 / 65535.0 * 255.0;
        assert!(
            (mean - truth).abs() < 0.5,
            "dithered block mean {mean} should track {truth} within half a level"
        );
    }

    #[test]
    fn rgba16_roundtrip_preserves_precision() {
        // A 16-bit image must survive from_rgba16 → flatten16 bit-exact.
        let (w, h) = (5u32, 3u32);
        let src: Vec<u16> = (0..(w * h * 4))
            .map(|i| (i as u16).wrapping_mul(517))
            .collect();
        let map = TileMap::from_rgba16(&src, w, h);
        assert!(map.has_hdr(), "16-bit map should report HDR");
        assert_eq!(map.flatten16(), src, "16-bit round-trip must be lossless");
    }

    #[test]
    fn rgba16_downconverts_to_8bit_mirror() {
        // The 8-bit mirror is the high byte of each 16-bit sample, so existing
        // 8-bit paths see a sensible image.
        let (w, h) = (2u32, 2u32);
        let src: Vec<u16> = vec![0xFFFF, 0x8000, 0x0100, 0xFF00].repeat(w as usize * h as usize);
        let map = TileMap::from_rgba16(&src, w, h);
        let (r, g, b, a) = map.get_pixel(0, 0);
        assert_eq!((r, g, b, a), (0xFF, 0x80, 0x01, 0xFF));
    }

    #[test]
    fn eight_bit_path_has_no_hdr() {
        // The default 8-bit constructor carries no 16-bit master (zero overhead,
        // unchanged behaviour).
        let map = TileMap::from_rgba(&[10, 20, 30, 255], 1, 1);
        assert!(!map.has_hdr());
    }

    #[test]
    fn editing_a_16bit_tile_drops_its_hdr_master() {
        // An 8-bit mutation invalidates the 16-bit master so export can't ship
        // stale precision.
        let (w, h) = (4u32, 4u32);
        let src: Vec<u16> = vec![0x4000u16; (w * h * 4) as usize];
        let mut map = TileMap::from_rgba16(&src, w, h);
        assert!(map.has_hdr());
        map.set_pixel(0, 0, 1, 2, 3, 255);
        assert!(!map.has_hdr(), "8-bit edit must drop the HDR master");
    }
}
