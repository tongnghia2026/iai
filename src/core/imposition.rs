// "Xếp ảnh in" — grid imposition of ID photos onto a print sheet.
//
// Pure layout math. Paper/photo presets are fixed pixel sizes at SHEET_DPI so
// the printed result is exact; the grid tries both orientations and keeps the
// one that fits more copies (ID shops lay 3×4 prints sideways: 10 per 10×15,
// 18 per 13×18). The app layer (app/actions/impose.rs) turns a layout into a
// new document with one layer per copy so the user can rearrange by hand.

/// Print sheets are generated at this resolution.
pub const SHEET_DPI: f32 = 600.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Paper {
    /// 10×15 cm photo paper.
    P10x15,
    /// 13×18 cm photo paper.
    P13x18,
}

impl Paper {
    /// Sheet size in pixels at `SHEET_DPI`, portrait.
    pub fn size_px(self) -> (u32, u32) {
        match self {
            Paper::P10x15 => (2362, 3543),
            Paper::P13x18 => (3071, 4252),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Paper::P10x15 => "10×15",
            Paper::P13x18 => "13×18",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhotoKind {
    /// "3×4" ID photo as shops actually cut it: 2.8×3.8 cm.
    Id3x4,
    /// Full-size 4×6 cm photo.
    Id4x6,
}

impl PhotoKind {
    /// Photo cell in pixels at `SHEET_DPI`, portrait (before any rotation).
    pub fn cell_px(self) -> (u32, u32) {
        match self {
            PhotoKind::Id3x4 => (661, 898),  // 2.8 × 3.8 cm
            PhotoKind::Id4x6 => (945, 1417), // 4 × 6 cm
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            PhotoKind::Id3x4 => "3×4",
            PhotoKind::Id4x6 => "4×6",
        }
    }

    /// Physical cell size in centimetres.
    pub fn cell_cm(self) -> (f32, f32) {
        match self {
            PhotoKind::Id3x4 => (2.8, 3.8),
            PhotoKind::Id4x6 => (4.0, 6.0),
        }
    }

    /// Detect which preset a document matches from its pixel size + DPI by
    /// comparing physical sizes (so 300dpi and 600dpi crops both match).
    pub fn detect(w: u32, h: u32, dpi: f32) -> Option<PhotoKind> {
        if w == 0 || h == 0 || !(1.0..=10000.0).contains(&dpi) {
            return None;
        }
        let (w_cm, h_cm) = (w as f32 / dpi * 2.54, h as f32 / dpi * 2.54);
        const TOL_CM: f32 = 0.12;
        for kind in [PhotoKind::Id3x4, PhotoKind::Id4x6] {
            let (cw, ch) = kind.cell_cm();
            if (w_cm - cw).abs() <= TOL_CM && (h_cm - ch).abs() <= TOL_CM {
                return Some(kind);
            }
        }
        None
    }
}

/// A computed sheet layout: where each copy goes and at what final size.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    /// Top-left corner of each placed copy, row-major.
    pub placements: Vec<(u32, u32)>,
    /// Final placed size of one copy on the sheet (after optional rotation).
    pub cell_w: u32,
    pub cell_h: u32,
    /// True when copies are rotated 90° (landscape) on the sheet.
    pub rotated: bool,
}

/// Grid of photo copies centred on the sheet, using whichever orientation
/// (upright or rotated 90°) fits more copies. Empty when nothing fits.
pub fn layout(paper: Paper, kind: PhotoKind, gap: u32) -> Layout {
    let (paper_w, paper_h) = paper.size_px();
    let (cw, ch) = kind.cell_px();
    let upright = grid(paper_w, paper_h, cw, ch, gap);
    let sideways = grid(paper_w, paper_h, ch, cw, gap);
    if sideways.0 * sideways.1 > upright.0 * upright.1 {
        build(paper_w, paper_h, ch, cw, gap, sideways, true)
    } else {
        build(paper_w, paper_h, cw, ch, gap, upright, false)
    }
}

/// How many (cols, rows) of a `cw`×`ch` cell fit on the sheet with `gap`
/// pixels between cells.
fn grid(paper_w: u32, paper_h: u32, cw: u32, ch: u32, gap: u32) -> (u32, u32) {
    let fit = |paper: u32, cell: u32| -> u32 {
        if cell == 0 || cell > paper {
            return 0;
        }
        (paper + gap) / (cell + gap)
    };
    (fit(paper_w, cw), fit(paper_h, ch))
}

fn build(
    paper_w: u32,
    paper_h: u32,
    cell_w: u32,
    cell_h: u32,
    gap: u32,
    (cols, rows): (u32, u32),
    rotated: bool,
) -> Layout {
    let mut placements = Vec::new();
    if cols > 0 && rows > 0 {
        let block_w = cols * cell_w + (cols - 1) * gap;
        let block_h = rows * cell_h + (rows - 1) * gap;
        let x0 = (paper_w - block_w) / 2;
        let y0 = (paper_h - block_h) / 2;
        for r in 0..rows {
            for c in 0..cols {
                placements.push((x0 + c * (cell_w + gap), y0 + r * (cell_h + gap)));
            }
        }
    }
    Layout {
        placements,
        cell_w,
        cell_h,
        rotated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ten_3x4_on_10x15() {
        let l = layout(Paper::P10x15, PhotoKind::Id3x4, 10);
        assert_eq!(l.placements.len(), 10); // 5 rows × 2 sideways
        assert!(l.rotated);
        assert_eq!((l.cell_w, l.cell_h), (898, 661));
    }

    #[test]
    fn eighteen_3x4_on_13x18() {
        let l = layout(Paper::P13x18, PhotoKind::Id3x4, 10);
        assert_eq!(l.placements.len(), 18); // 6 rows × 3 sideways
        assert!(l.rotated);
    }

    #[test]
    fn eight_4x6_on_13x18() {
        let l = layout(Paper::P13x18, PhotoKind::Id4x6, 10);
        assert_eq!(l.placements.len(), 8); // 4 rows × 2 sideways
        assert!(l.rotated);
        assert_eq!((l.cell_w, l.cell_h), (1417, 945));
    }

    #[test]
    fn placements_stay_inside_the_sheet() {
        for paper in [Paper::P10x15, Paper::P13x18] {
            for kind in [PhotoKind::Id3x4, PhotoKind::Id4x6] {
                for gap in [0u32, 10, 100] {
                    let (pw, ph) = paper.size_px();
                    let l = layout(paper, kind, gap);
                    for &(x, y) in &l.placements {
                        assert!(x + l.cell_w <= pw, "{paper:?} {kind:?} gap {gap}");
                        assert!(y + l.cell_h <= ph, "{paper:?} {kind:?} gap {gap}");
                    }
                }
            }
        }
    }

    #[test]
    fn copies_never_overlap() {
        let l = layout(Paper::P10x15, PhotoKind::Id3x4, 10);
        for (i, &(ax, ay)) in l.placements.iter().enumerate() {
            for &(bx, by) in l.placements.iter().skip(i + 1) {
                let disjoint_x = ax + l.cell_w + 10 <= bx || bx + l.cell_w + 10 <= ax;
                let disjoint_y = ay + l.cell_h + 10 <= by || by + l.cell_h + 10 <= ay;
                assert!(disjoint_x || disjoint_y);
            }
        }
    }

    #[test]
    fn detect_matches_600dpi_and_300dpi_crops() {
        assert_eq!(PhotoKind::detect(661, 898, 600.0), Some(PhotoKind::Id3x4));
        assert_eq!(PhotoKind::detect(331, 449, 300.0), Some(PhotoKind::Id3x4));
        assert_eq!(PhotoKind::detect(945, 1417, 600.0), Some(PhotoKind::Id4x6));
        assert_eq!(PhotoKind::detect(4000, 6000, 72.0), None);
        assert_eq!(PhotoKind::detect(661, 898, 0.0), None);
    }
}
