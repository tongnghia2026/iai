// "Xếp ảnh in" — grid imposition of ID photos onto a print sheet.
//
// Pure layout math. Paper/photo presets are fixed pixel sizes at SHEET_DPI so
// the printed result is exact; the grid tries both orientations and keeps the
// one that fits more copies (ID shops lay 3×4 prints sideways: 10 per 10×15,
// 18 per 13×18). A mixed sheet holds two sizes; a folder of photos fills as
// many sheets as it takes, a few copies of each. The app layer
// (app/actions/impose.rs) turns a sheet into a new document with one layer
// per copy so the user can rearrange by hand.

use serde::{Deserialize, Serialize};

/// Print sheets are generated at this resolution.
pub const SHEET_DPI: f32 = 600.0;

/// Width of the cutting line drawn around a photo on white, which white
/// paper would otherwise hide.
pub const BORDER_PX: u32 = 2;
/// Its colour.
pub const BORDER_RGB: [u8; 3] = [224, 40, 40];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Paper {
    /// 10×15 cm photo paper.
    P10x15,
    /// 13×18 cm photo paper.
    P13x18,
}

impl Paper {
    pub const ALL: [Paper; 2] = [Paper::P10x15, Paper::P13x18];

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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PhotoKind {
    /// 2×3 cm.
    Id2x3,
    /// "3×4" ID photo as shops actually cut it: 2.8×3.8 cm.
    Id3x4,
    /// Full-size 4×6 cm photo.
    Id4x6,
}

impl PhotoKind {
    pub const ALL: [PhotoKind; 3] = [PhotoKind::Id2x3, PhotoKind::Id3x4, PhotoKind::Id4x6];

    /// Photo cell in pixels at `SHEET_DPI`, portrait (before any rotation).
    pub fn cell_px(self) -> (u32, u32) {
        match self {
            PhotoKind::Id2x3 => (472, 709),  // 2 × 3 cm
            PhotoKind::Id3x4 => (661, 898),  // 2.8 × 3.8 cm
            PhotoKind::Id4x6 => (945, 1417), // 4 × 6 cm
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            PhotoKind::Id2x3 => "2×3",
            PhotoKind::Id3x4 => "3×4",
            PhotoKind::Id4x6 => "4×6",
        }
    }

    /// Physical cell size in centimetres.
    pub fn cell_cm(self) -> (f32, f32) {
        match self {
            PhotoKind::Id2x3 => (2.0, 3.0),
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
        for kind in PhotoKind::ALL {
            let (cw, ch) = kind.cell_cm();
            if (w_cm - cw).abs() <= TOL_CM && (h_cm - ch).abs() <= TOL_CM {
                return Some(kind);
            }
        }
        None
    }
}

/// The plain colour a cut-out person is laid on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Backdrop {
    White,
    Blue,
}

impl Backdrop {
    pub fn rgb(self) -> [u8; 3] {
        match self {
            Backdrop::White => [255, 255, 255],
            Backdrop::Blue => [0, 144, 255],
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Backdrop::White => "Trắng",
            Backdrop::Blue => "Xanh",
        }
    }
}

/// What "Xếp ảnh in" remembers: the cutting gap between copies (px at
/// [`SHEET_DPI`]), for the mixed sheet each size's backdrop, and for a folder
/// of photos their size and how many prints of each. A sheet of one size
/// prints the photo on the background it has.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SheetOptions {
    pub gap: u32,
    pub backdrop_3x4: Backdrop,
    pub backdrop_4x6: Backdrop,
    pub folder_kind: PhotoKind,
    pub folder_copies: u32,
}

impl Default for SheetOptions {
    fn default() -> Self {
        Self {
            gap: 10,
            backdrop_3x4: Backdrop::Blue,
            backdrop_4x6: Backdrop::White,
            folder_kind: PhotoKind::Id3x4,
            folder_copies: 2,
        }
    }
}

impl SheetOptions {
    /// The mixed sheet's backdrop for `kind`.
    pub fn backdrop(&self, kind: PhotoKind) -> Backdrop {
        match kind {
            PhotoKind::Id4x6 => self.backdrop_4x6,
            PhotoKind::Id2x3 | PhotoKind::Id3x4 => self.backdrop_3x4,
        }
    }
}

/// What goes on a sheet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sheet {
    /// As many copies of one size as fit.
    Grid(Paper, PhotoKind),
    /// 13×18 with six 3×4 over two 4×6, from the top; the rest stays blank.
    Mixed,
}

/// The copies of one size on a sheet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub kind: PhotoKind,
    pub layout: Layout,
}

impl Sheet {
    pub const ALL: [Sheet; 7] = [
        Sheet::Grid(Paper::P10x15, PhotoKind::Id2x3),
        Sheet::Grid(Paper::P13x18, PhotoKind::Id2x3),
        Sheet::Grid(Paper::P10x15, PhotoKind::Id3x4),
        Sheet::Grid(Paper::P13x18, PhotoKind::Id3x4),
        Sheet::Grid(Paper::P10x15, PhotoKind::Id4x6),
        Sheet::Grid(Paper::P13x18, PhotoKind::Id4x6),
        Sheet::Mixed,
    ];

    pub fn paper(self) -> Paper {
        match self {
            Sheet::Grid(paper, _) => paper,
            Sheet::Mixed => Paper::P13x18,
        }
    }

    /// Where every copy goes, one block per size; blocks without a copy are
    /// left out.
    pub fn blocks(self, gap: u32) -> Vec<Block> {
        let blocks = match self {
            Sheet::Grid(paper, kind) => vec![Block {
                kind,
                layout: layout(paper, kind, gap),
            }],
            Sheet::Mixed => mixed(self.paper(), gap),
        };
        blocks
            .into_iter()
            .filter(|b| !b.layout.placements.is_empty())
            .collect()
    }

    /// "6 tấm 3×4 + 2 tấm 4×6"; `None` when nothing fits.
    pub fn copies(self, gap: u32) -> Option<String> {
        let parts: Vec<String> = self
            .blocks(gap)
            .iter()
            .map(|b| format!("{} tấm {}", b.layout.placements.len(), b.kind.label()))
            .collect();
        (!parts.is_empty()).then(|| parts.join(" + "))
    }

    /// "13×18 — 6 tấm 3×4 + 2 tấm 4×6"; `None` when nothing fits.
    pub fn label(self, gap: u32) -> Option<String> {
        self.copies(gap)
            .map(|copies| format!("{} — {copies}", self.paper().label()))
    }

    /// How many copies the sheet holds.
    pub fn count(self, gap: u32) -> usize {
        self.blocks(gap)
            .iter()
            .map(|b| b.layout.placements.len())
            .sum()
    }
}

/// The most prints of each photo a folder can ask for.
pub const MAX_FOLDER_COPIES: u32 = 20;

/// A folder of photos, one person each, on as many sheets of one paper as it
/// takes: `copies` prints of every photo, all of one size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FolderSheets {
    pub paper: Paper,
    pub kind: PhotoKind,
    pub copies: u32,
}

impl FolderSheets {
    /// Prints one sheet holds.
    pub fn per_sheet(self, gap: u32) -> usize {
        layout(self.paper, self.kind, gap).placements.len()
    }

    /// Sheets that `photos` photos take; 0 when a sheet holds none.
    pub fn sheets(self, photos: usize, gap: u32) -> usize {
        match self.per_sheet(gap) {
            0 => 0,
            per_sheet => (photos * self.copies as usize).div_ceil(per_sheet),
        }
    }
}

/// The order in which a sheet's cells are filled when every photo comes in
/// `copies`: along the rows, or down the columns when only a column holds
/// whole sets, so that one person's prints lie side by side and the end of a
/// line does not part them.
pub fn fill_order(layout: &Layout, copies: usize) -> Vec<usize> {
    let cells = layout.placements.len();
    let Some(&(_, top)) = layout.placements.first() else {
        return Vec::new();
    };
    let cols = layout
        .placements
        .iter()
        .take_while(|&&(_, y)| y == top)
        .count();
    let rows = cells / cols;
    let whole = |line: usize| copies > 0 && line % copies == 0;
    if !whole(cols) && whole(rows) {
        (0..cols)
            .flat_map(|c| (0..rows).map(move |r| r * cols + c))
            .collect()
    } else {
        (0..cells).collect()
    }
}

/// The mixed sheet: both sizes sideways, two rows of three 3×4 over a row of
/// two 4×6, with one margin at the left and the top (the wider row centred).
fn mixed(paper: Paper, gap: u32) -> Vec<Block> {
    let (paper_w, paper_h) = paper.size_px();
    let (small_h, small_w) = PhotoKind::Id3x4.cell_px();
    let (large_h, large_w) = PhotoKind::Id4x6.cell_px();
    let width = (2 * large_w + gap).max(3 * small_w + 2 * gap);
    let height = 2 * (small_h + gap) + large_h;
    if width > paper_w || height > paper_h {
        return Vec::new();
    }
    let margin = ((paper_w - width) / 2).min(paper_h - height);
    let row = |count: u32, cell_w: u32, y: u32| -> Vec<(u32, u32)> {
        (0..count)
            .map(|c| (margin + c * (cell_w + gap), y))
            .collect()
    };
    let mut small = row(3, small_w, margin);
    small.extend(row(3, small_w, margin + small_h + gap));
    let large = row(2, large_w, margin + 2 * (small_h + gap));
    vec![
        Block {
            kind: PhotoKind::Id3x4,
            layout: Layout {
                placements: small,
                cell_w: small_w,
                cell_h: small_h,
                rotated: true,
            },
        },
        Block {
            kind: PhotoKind::Id4x6,
            layout: Layout {
                placements: large,
                cell_w: large_w,
                cell_h: large_h,
                rotated: true,
            },
        },
    ]
}

/// Straight-alpha RGBA laid over a plain colour: opaque RGBA.
pub fn over_backdrop(rgba: &[u8], colour: [u8; 3]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgba.len());
    for px in rgba.chunks_exact(4) {
        let a = px[3] as u32;
        for k in 0..3 {
            out.push(((px[k] as u32 * a + colour[k] as u32 * (255 - a) + 127) / 255) as u8);
        }
        out.push(255);
    }
    out
}

/// Whether a photo's rim is white: its edge would vanish on white paper.
pub fn rim_is_white(rgba: &[u8], width: u32, height: u32) -> bool {
    let (w, h) = (width as usize, height as usize);
    if w == 0 || h == 0 || rgba.len() != w * h * 4 {
        return false;
    }
    let white = |x: usize, y: usize| {
        let o = (y * w + x) * 4;
        rgba[o..o + 3].iter().all(|&v| v >= 244)
    };
    let rim = (0..w)
        .flat_map(|x| [(x, 0), (x, h - 1)])
        .chain((0..h).flat_map(|y| [(0, y), (w - 1, y)]));
    let (mut total, mut whites) = (0usize, 0usize);
    for (x, y) in rim {
        total += 1;
        whites += white(x, y) as usize;
    }
    // The person's shoulders reach the bottom edge.
    whites * 10 >= total * 6
}

/// A photo with a cutting line of `border` px around it, `border` px larger
/// on every side.
pub fn framed(rgba: &[u8], width: u32, height: u32, border: u32) -> (Vec<u8>, u32, u32) {
    let (w, h, b) = (width as usize, height as usize, border as usize);
    let (fw, fh) = (w + 2 * b, h + 2 * b);
    let [r, g, bl] = BORDER_RGB;
    let mut out = [r, g, bl, 255].repeat(fw * fh);
    for y in 0..h {
        let to = ((y + b) * fw + b) * 4;
        out[to..to + w * 4].copy_from_slice(&rgba[y * w * 4..(y + 1) * w * 4]);
    }
    (out, fw as u32, fh as u32)
}

/// A photo with the cutting line drawn over its own outermost `border` px:
/// for a sheet with no gap to draw it in.
pub fn outlined(rgba: &mut [u8], width: u32, height: u32, border: u32) {
    let (w, h, b) = (width as usize, height as usize, border as usize);
    let [r, g, bl] = BORDER_RGB;
    for y in 0..h {
        for x in 0..w {
            if x < b || y < b || x + b >= w || y + b >= h {
                let o = (y * w + x) * 4;
                rgba[o..o + 4].copy_from_slice(&[r, g, bl, 255]);
            }
        }
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
    fn every_size_has_a_sheet_on_both_papers() {
        // What fits with the usual gap; 2×3 and 3×4 lie sideways.
        let counts: Vec<(usize, bool)> = Sheet::ALL
            .iter()
            .filter_map(|sheet| match *sheet {
                Sheet::Grid(paper, kind) => {
                    let l = layout(paper, kind, 10);
                    Some((l.placements.len(), l.rotated))
                }
                Sheet::Mixed => None,
            })
            .collect();
        assert_eq!(
            counts,
            vec![
                (21, true),
                (32, true),
                (10, true),
                (18, true),
                (4, false),
                (8, true)
            ]
        );
        assert_eq!(Sheet::Grid(Paper::P10x15, PhotoKind::Id2x3).count(10), 21);
        assert_eq!(Sheet::Mixed.count(10), 8);
        assert_eq!(
            Sheet::Grid(Paper::P10x15, PhotoKind::Id4x6)
                .label(10)
                .as_deref(),
            Some("10×15 — 4 tấm 4×6")
        );
        assert_eq!(
            Sheet::Mixed.copies(10).as_deref(),
            Some("6 tấm 3×4 + 2 tấm 4×6")
        );
    }

    #[test]
    fn placements_stay_inside_the_sheet() {
        for paper in Paper::ALL {
            for kind in PhotoKind::ALL {
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
    fn the_mixed_sheet_is_six_3x4_over_two_4x6() {
        let blocks = Sheet::Mixed.blocks(10);
        let (pw, ph) = Sheet::Mixed.paper().size_px();
        assert_eq!(blocks.len(), 2);
        let (small, large) = (&blocks[0].layout, &blocks[1].layout);
        assert_eq!(
            (blocks[0].kind, small.placements.len()),
            (PhotoKind::Id3x4, 6)
        );
        assert_eq!(
            (blocks[1].kind, large.placements.len()),
            (PhotoKind::Id4x6, 2)
        );
        assert!(small.rotated && large.rotated);
        assert_eq!((large.cell_w, large.cell_h), (1417, 945));
        // One margin at the left and the top, about half a centimetre.
        let margin = small.placements[0].0;
        assert_eq!(small.placements[0], (margin, margin));
        assert_eq!(large.placements[0].0, margin);
        assert!((100..=130).contains(&margin), "{margin}");
        // The 4×6 row is centred and sits a gap below the 3×4 rows.
        let right = large.placements[1].0 + large.cell_w;
        assert!((pw - right).abs_diff(margin) <= 1);
        let below = small.placements[5].1 + small.cell_h + 10;
        assert_eq!(large.placements[0].1, below);
        // Nothing overlaps or leaves the sheet.
        let cells: Vec<(u32, u32, u32, u32)> = blocks
            .iter()
            .flat_map(|b| {
                let l = &b.layout;
                l.placements
                    .iter()
                    .map(|&(x, y)| (x, y, l.cell_w, l.cell_h))
            })
            .collect();
        for (i, a) in cells.iter().enumerate() {
            assert!(a.0 + a.2 <= pw && a.1 + a.3 <= ph);
            for b in cells.iter().skip(i + 1) {
                let apart_x = a.0 + a.2 + 10 <= b.0 || b.0 + b.2 + 10 <= a.0;
                let apart_y = a.1 + a.3 + 10 <= b.1 || b.1 + b.3 + 10 <= a.1;
                assert!(apart_x || apart_y, "{a:?} {b:?}");
            }
        }
        assert_eq!(
            Sheet::Mixed.label(10).as_deref(),
            Some("13×18 — 6 tấm 3×4 + 2 tấm 4×6")
        );
        assert_eq!(
            Sheet::Grid(Paper::P10x15, PhotoKind::Id3x4)
                .label(10)
                .as_deref(),
            Some("10×15 — 10 tấm 3×4")
        );
    }

    #[test]
    fn a_cut_out_takes_its_backdrop_and_white_ones_a_cutting_line() {
        // Opaque red, half-clear red, clear.
        let cut_out = [200u8, 0, 0, 255, 200, 0, 0, 128, 9, 9, 9, 0];
        let blue = over_backdrop(&cut_out, Backdrop::Blue.rgb());
        assert_eq!(&blue[0..4], &[200, 0, 0, 255]);
        // The owner's blue, #0090FF.
        assert_eq!(&blue[8..12], &[0, 144, 255, 255]);
        assert!(blue[4] > 95 && blue[4] < 110 && blue[6] > 115, "{blue:?}");

        let white = over_backdrop(&[0u8; 4 * 6], Backdrop::White.rgb());
        assert!(rim_is_white(&white, 3, 2));
        let on_blue = over_backdrop(&[0u8; 4 * 6], Backdrop::Blue.rgb());
        assert!(!rim_is_white(&on_blue, 3, 2));
        let (out, w, h) = framed(&white, 3, 2, 2);
        assert_eq!((w, h), (7, 6));
        let at = |x: usize, y: usize| &out[(y * 7 + x) * 4..(y * 7 + x) * 4 + 3];
        assert_eq!(at(0, 0), &BORDER_RGB);
        assert_eq!(at(1, 3), &BORDER_RGB);
        assert_eq!(at(2, 2), &[255, 255, 255]);
        assert_eq!(at(4, 3), &[255, 255, 255]);
        assert_eq!(at(5, 3), &BORDER_RGB);

        let mut photo = over_backdrop(&[0u8; 4 * 25], Backdrop::White.rgb());
        outlined(&mut photo, 5, 5, 1);
        assert_eq!(&photo[0..3], &BORDER_RGB);
        let centre = (2 * 5 + 2) * 4;
        assert_eq!(&photo[centre..centre + 3], &[255, 255, 255]);
    }

    #[test]
    fn a_folder_takes_as_many_sheets_as_its_prints_need() {
        let on = |paper| FolderSheets {
            paper,
            kind: PhotoKind::Id3x4,
            copies: 2,
        };
        // Eighteen people, two prints each.
        assert_eq!(on(Paper::P13x18).per_sheet(10), 18);
        assert_eq!(on(Paper::P13x18).sheets(18, 10), 2);
        assert_eq!(on(Paper::P10x15).sheets(18, 10), 4);
        assert_eq!(on(Paper::P10x15).sheets(5, 10), 1);
        assert_eq!(on(Paper::P10x15).sheets(0, 10), 0);
        // A gap that leaves room for one print: a sheet each.
        let apart = FolderSheets {
            kind: PhotoKind::Id4x6,
            ..on(Paper::P10x15)
        };
        assert_eq!(apart.per_sheet(3000), 1);
        assert_eq!(apart.sheets(3, 3000), 6);
    }

    #[test]
    fn a_persons_prints_lie_together_on_the_sheet() {
        // Two across: each row is one person's pair.
        let narrow = layout(Paper::P10x15, PhotoKind::Id3x4, 10);
        assert_eq!(fill_order(&narrow, 2), (0..10).collect::<Vec<_>>());
        // Three across, six down: pairs run down the columns.
        let wide = layout(Paper::P13x18, PhotoKind::Id3x4, 10);
        let order = fill_order(&wide, 2);
        assert_eq!(&order[..7], &[0, 3, 6, 9, 12, 15, 1]);
        for pair in order.chunks(2) {
            let (a, b) = (wide.placements[pair[0]], wide.placements[pair[1]]);
            assert_eq!(a.0, b.0, "one column");
            assert_eq!(b.1 - a.1, wide.cell_h + 10, "one under the other");
        }
        // Three of each fill a row; four fit neither way: along the rows.
        assert_eq!(fill_order(&wide, 3), (0..18).collect::<Vec<_>>());
        assert_eq!(fill_order(&wide, 4), (0..18).collect::<Vec<_>>());
        // Every cell once, whatever the count.
        for copies in 0..8 {
            let mut cells = fill_order(&wide, copies);
            cells.sort_unstable();
            assert_eq!(cells, (0..18).collect::<Vec<_>>());
        }
        let none = Layout {
            placements: Vec::new(),
            cell_w: 1,
            cell_h: 1,
            rotated: false,
        };
        assert!(fill_order(&none, 2).is_empty());
    }

    #[test]
    fn sheet_options_read_back_and_default_to_blue_3x4_white_4x6() {
        let d = SheetOptions::default();
        assert_eq!(d.backdrop(PhotoKind::Id3x4), Backdrop::Blue);
        assert_eq!(d.backdrop(PhotoKind::Id4x6), Backdrop::White);
        assert_eq!((d.folder_kind, d.folder_copies), (PhotoKind::Id3x4, 2));
        let partial: SheetOptions = serde_json::from_str("{\"gap\": 20}").unwrap();
        assert_eq!(partial, SheetOptions { gap: 20, ..d });
        let json = serde_json::to_string(&d).unwrap();
        assert_eq!(serde_json::from_str::<SheetOptions>(&json).unwrap(), d);
    }

    #[test]
    fn detect_matches_600dpi_and_300dpi_crops() {
        assert_eq!(PhotoKind::detect(661, 898, 600.0), Some(PhotoKind::Id3x4));
        assert_eq!(PhotoKind::detect(331, 449, 300.0), Some(PhotoKind::Id3x4));
        assert_eq!(PhotoKind::detect(945, 1417, 600.0), Some(PhotoKind::Id4x6));
        assert_eq!(PhotoKind::detect(472, 709, 600.0), Some(PhotoKind::Id2x3));
        assert_eq!(PhotoKind::detect(4000, 6000, 72.0), None);
        assert_eq!(PhotoKind::detect(661, 898, 0.0), None);
    }
}
