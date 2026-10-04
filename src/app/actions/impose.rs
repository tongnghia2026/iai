// "Xếp ảnh in" — build a print-sheet document from the active photo.
//
// The active document's flattened composite is scaled once per photo size to
// the exact cell (aspect-preserving cover crop), rotated if the layout says
// so, and stamped into a NEW white 600dpi document: one layer per copy, one
// group per size, so the user can move/delete/duplicate prints by hand.
//
// A sheet of one size prints the photo on the background it has. On the mixed
// sheet a person cut out over a plain Background layer (as "Làm ảnh thẻ"
// leaves the photo) is laid on each size's own backdrop. Photos on white get
// a cutting line around them.

use crate::app::render::CanvasEvent;
use crate::app::state::App;
use crate::core::canvas::Canvas;
use crate::core::imposition::{self, Backdrop, PhotoKind, Sheet, SheetOptions, SHEET_DPI};
use crate::core::layer::Layer;
use crate::core::tile::TileMap;

/// The photo without its backdrop, as straight-alpha RGBA: when the bottom
/// layer is a Background of one flat colour and what lies above it is a
/// cut-out (part clear, part solid).
fn cut_out(canvas: &Canvas) -> Option<Vec<u8>> {
    let stack = &canvas.layer_stack;
    let background = stack
        .layers
        .first()
        .filter(|l| l.is_background && l.visible)?;
    let fill = background.flatten_tiles();
    let first = fill.get(0..4).filter(|px| px[3] == 255)?;
    if fill.chunks_exact(4).any(|px| px != first) {
        return None;
    }
    let mut above = stack.clone();
    above.layers[0].visible = false;
    let rgba = above.flatten(canvas.width, canvas.height);
    let clear = rgba.chunks_exact(4).any(|px| px[3] < 16);
    let solid = rgba.chunks_exact(4).any(|px| px[3] > 239);
    (clear && solid).then_some(rgba)
}

/// One size's print: its pixels as placed on the sheet, and how far they
/// reach past the photo on every side (the cutting line).
struct Stamp {
    tiles: TileMap,
    size: (u32, u32),
    pad: u32,
    /// The source's shape was off this size's: its sides or ends were cropped.
    cropped: bool,
}

/// `photo` (opaque RGBA) as one print of `kind`: scaled to cover the cell and
/// centre-cropped, with a cutting line when it stands on white, turned
/// sideways when the layout is.
fn stamp(
    photo: image::RgbaImage,
    kind: PhotoKind,
    on_white: Option<bool>,
    gap: u32,
    rotated: bool,
) -> Stamp {
    let (src_w, src_h) = photo.dimensions();
    let (cell_w, cell_h) = kind.cell_px();
    let src_ratio = src_w as f32 / src_h as f32;
    let cell_ratio = cell_w as f32 / cell_h as f32;
    let cropped = (src_ratio - cell_ratio).abs() / cell_ratio > 0.02;
    let s = (cell_w as f32 / src_w as f32).max(cell_h as f32 / src_h as f32);
    let (fw, fh) = (
        ((src_w as f32 * s).round() as u32).max(cell_w),
        ((src_h as f32 * s).round() as u32).max(cell_h),
    );
    // A photo already at the cell's scale gives its own pixels.
    let scaled = if (fw, fh) == (src_w, src_h) {
        photo
    } else {
        image::imageops::resize(&photo, fw, fh, image::imageops::FilterType::Lanczos3)
    };
    let img = if (fw, fh) == (cell_w, cell_h) {
        scaled
    } else {
        image::imageops::crop_imm(
            &scaled,
            (fw - cell_w) / 2,
            (fh - cell_h) / 2,
            cell_w,
            cell_h,
        )
        .to_image()
    };
    let mut rgba = img.into_raw();
    let (mut w, mut h, mut pad) = (cell_w, cell_h, 0);
    if on_white.unwrap_or_else(|| imposition::rim_is_white(&rgba, w, h)) {
        // The line lies in the cutting gap, clear of the photo, when the gap
        // has room for two of them.
        if gap >= 2 * imposition::BORDER_PX {
            pad = imposition::BORDER_PX;
            (rgba, w, h) = imposition::framed(&rgba, w, h, pad);
        } else {
            imposition::outlined(&mut rgba, w, h, imposition::BORDER_PX);
        }
    }
    let img = image::RgbaImage::from_raw(w, h, rgba).expect("sized above");
    let img = if rotated {
        image::imageops::rotate90(&img)
    } else {
        img
    };
    let size = img.dimensions();
    Stamp {
        tiles: TileMap::from_rgba(img.as_raw(), size.0, size.1),
        size,
        pad,
        cropped,
    }
}

impl App {
    pub(crate) fn do_impose_sheet(&mut self, sheet: Sheet, options: SheetOptions) {
        if self.modal_lock_active() {
            self.deny_modal_action();
            return;
        }
        if self.has_only_welcome_placeholder() {
            self.shell.status_msg = "Hãy mở ảnh đã crop trước".to_string();
            return;
        }

        let gap = options.gap;
        let blocks = sheet.blocks(gap);
        let Some(label) = sheet.label(gap) else {
            self.shell.status_msg = "Không xếp được — khe cắt quá lớn".to_string();
            return;
        };

        let src = &self.docs.documents[self.docs.active_doc_idx].canvas;
        let (src_w, src_h) = (src.width, src.height);
        if src_w == 0 || src_h == 0 {
            return;
        }
        let mixed = sheet == Sheet::Mixed;
        let person = if mixed { cut_out(src) } else { None };
        let flat = src.flatten_for_export();
        let mut stamps = Vec::with_capacity(blocks.len());
        for block in &blocks {
            let (rgba, on_white) = match &person {
                Some(person) => {
                    let backdrop = options.backdrop(block.kind);
                    (
                        imposition::over_backdrop(person, backdrop.rgb()),
                        Some(backdrop == Backdrop::White),
                    )
                }
                None => (flat.clone(), None),
            };
            let Some(photo) = image::RgbaImage::from_raw(src_w, src_h, rgba) else {
                self.shell.status_msg = "Không đọc được ảnh nguồn".to_string();
                return;
            };
            stamps.push(stamp(
                photo,
                block.kind,
                on_white,
                gap,
                block.layout.rotated,
            ));
        }

        // New white sheet document at SHEET_DPI (same construction as do_new_tab).
        let (paper_w, paper_h) = sheet.paper().size_px();
        let id = crate::core::document::DocumentId(self.docs.next_doc_id);
        self.docs.next_doc_id += 1;
        let mut canvas = Canvas::new(paper_w, paper_h);
        canvas.metadata.resolution_ppi = SHEET_DPI;

        // Background (id 0), then per size one layer per copy inside a group.
        // Children sit contiguously below their header in the stack (same
        // shape as LayerStack group commands).
        {
            let stack = &mut canvas.layer_stack;
            stack.layers[0].is_background = true;
            let mut next_id = 1u32;
            for (block, stamp) in blocks.iter().zip(&stamps) {
                let group_id = next_id + block.layout.placements.len() as u32;
                let pad = stamp.pad as i32;
                for (i, &(x, y)) in block.layout.placements.iter().enumerate() {
                    let name = format!("Ảnh {}", i + 1);
                    let mut layer = Layer::new(next_id, &name, stamp.size.0, stamp.size.1);
                    layer.tiles = stamp.tiles.clone();
                    layer.offset = (x as i32 - pad, y as i32 - pad);
                    layer.parent_id = Some(group_id);
                    stack.layers.push(layer);
                    next_id += 1;
                }
                let group_name = format!("Ảnh thẻ {}", block.kind.label());
                stack
                    .layers
                    .push(Layer::new_group(group_id, &group_name, paper_w, paper_h));
                next_id = group_id + 1;
            }
            let top = stack.layers.len() - 1;
            stack.layers[top].selected = true;
            stack.active_idx = top;
            stack.set_next_id(next_id);
        }
        canvas.pixels_stale = true;
        canvas.ensure_pixels();

        let mut doc = crate::core::document::Document::new(id, paper_w, paper_h);
        doc.canvas = canvas;
        doc.title = format!("Trang {label}");
        doc.path = None;

        self.docs.documents.push(doc);
        self.docs.active_doc_idx = self.docs.documents.len() - 1;
        self.touch_doc_mru();
        self.docs.current_file = None;
        self.shell.ui.show_welcome = false;
        if let Some(gpu) = &mut self.win.gpu {
            gpu.resize_canvas_texture(paper_w, paper_h);
            gpu.compositor.tile_atlas.clear();
            gpu.compositor.ping_initialized = false;
            gpu.compositor.last_result_is_ping = false;
        }
        self.fit_canvas_to_screen();
        self.push_canvas_uniforms();
        self.upload_full();
        self.upload_selection_mask();
        self.apply_canvas_event(CanvasEvent::LayerStructureChanged);

        let mut message = format!("Đã xếp trang {label}");
        let cropped: Vec<&str> = blocks
            .iter()
            .zip(&stamps)
            .filter(|(_, stamp)| stamp.cropped)
            .map(|(block, _)| block.kind.label())
            .collect();
        if !cropped.is_empty() {
            message.push_str(&format!(
                " — LƯU Ý: ảnh nguồn lệch tỷ lệ {}, đã cắt giữa cho vừa ô",
                cropped.join(" và ")
            ));
        }
        if mixed && person.is_none() {
            message.push_str(" — ảnh chưa tách nền nên giữ nguyên nền của ảnh");
        }
        self.shell.status_msg = message;
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::id_photo::{PRINT_PPI, PRINT_PX};
    use crate::core::imposition::{Paper, BORDER_RGB};

    /// A 3×4 photo as "Làm ảnh thẻ" leaves it: a white Background and the
    /// person (a block from a third down, the middle half) cut out above it.
    fn id_photo() -> Canvas {
        let (w, h) = PRINT_PX;
        let mut canvas = Canvas::new(w, h);
        canvas.metadata.resolution_ppi = PRINT_PPI;
        canvas.layer_stack.layers[0].is_background = true;
        let mut person = vec![0u8; (w * h * 4) as usize];
        for y in h / 3..h {
            for x in w / 4..3 * w / 4 {
                let o = ((y * w + x) * 4) as usize;
                person[o..o + 4].copy_from_slice(&[90, 80, 70, 255]);
            }
        }
        canvas
            .layer_stack
            .layers
            .push(Layer::from_rgba(1, "Layer 1", person, w, h));
        canvas
    }

    #[test]
    fn only_a_person_over_a_plain_background_is_a_cut_out() {
        let canvas = id_photo();
        let (w, h) = (canvas.width, canvas.height);
        let person = cut_out(&canvas).expect("a cut-out");
        let alpha = |x: u32, y: u32| person[((y * w + x) * 4 + 3) as usize];
        assert_eq!((alpha(2, 2), alpha(w / 2, h - 2)), (0, 255));

        // A flat photo has nothing above its Background.
        let mut flat = Canvas::new(w, h);
        flat.layer_stack.layers[0].is_background = true;
        assert!(cut_out(&flat).is_none());

        // A photo as Background is no backdrop, whatever lies over it.
        let mut scene = id_photo();
        let mut pixels = vec![255u8; (w * h * 4) as usize];
        pixels[0] = 10;
        let mut background = Layer::from_rgba(0, "Background", pixels, w, h);
        background.is_background = true;
        scene.layer_stack.layers[0] = background;
        assert!(cut_out(&scene).is_none());
    }

    #[test]
    fn a_stamp_fits_its_cell_and_takes_a_cutting_line_on_white() {
        let (w, h) = PhotoKind::Id3x4.cell_px();
        let white = image::RgbaImage::from_pixel(w, h, image::Rgba([255; 4]));
        // In the gap when there is one, sideways with the layout.
        let lined = stamp(white.clone(), PhotoKind::Id3x4, Some(true), 10, true);
        assert_eq!(
            (lined.size, lined.pad, lined.cropped),
            ((h + 4, w + 4), 2, false)
        );
        // None on a colour; a 3×4 photo is cropped into a 4×6 cell.
        let plain = stamp(white.clone(), PhotoKind::Id4x6, Some(false), 10, true);
        assert_eq!(
            (plain.size, plain.pad, plain.cropped),
            ((1417, 945), 0, true)
        );
        // A flat photo with a white rim is seen as one on white; without a
        // gap the line goes over its own edge.
        let tight = stamp(white, PhotoKind::Id3x4, None, 0, false);
        assert_eq!((tight.size, tight.pad), ((w, h), 0));
        assert_eq!(&tight.tiles.flatten()[0..3], &BORDER_RGB);
    }

    #[test]
    fn an_id_photo_gives_the_4x6_its_own_pixels_and_the_3x4_all_of_itself() {
        // Single-pixel detail, which any resampling would smear.
        let (w, h) = PRINT_PX;
        let fine = image::RgbaImage::from_fn(w, h, |x, y| {
            image::Rgba([
                (x % 251) as u8,
                (y % 241) as u8,
                ((x + y) % 2 * 255) as u8,
                255,
            ])
        });
        let (cell_w, cell_h) = PhotoKind::Id4x6.cell_px();
        let large = stamp(fine.clone(), PhotoKind::Id4x6, Some(false), 10, false);
        assert_eq!((large.size, large.cropped), ((cell_w, cell_h), true));
        let middle = image::imageops::crop_imm(&fine, (w - cell_w) / 2, 0, cell_w, cell_h);
        assert!(large.tiles.flatten() == middle.to_image().into_raw());

        let small = stamp(fine, PhotoKind::Id3x4, Some(false), 10, false);
        assert_eq!(
            (small.size, small.cropped),
            (PhotoKind::Id3x4.cell_px(), false)
        );
    }

    #[test]
    fn the_mixed_sheet_has_blue_3x4_and_lined_white_4x6() {
        let mut app = App::new();
        app.shell.ui.show_welcome = false;
        app.docs.documents[0].canvas = id_photo();
        let options = SheetOptions::default();
        app.do_impose_sheet(Sheet::Mixed, options);

        assert_eq!(app.docs.documents.len(), 2);
        let doc = &app.docs.documents[app.docs.active_doc_idx];
        assert_eq!(doc.title, "Trang 13×18 — 6 tấm 3×4 + 2 tấm 4×6");
        let (pw, ph) = Paper::P13x18.size_px();
        assert_eq!((doc.canvas.width, doc.canvas.height), (pw, ph));
        assert_eq!(doc.canvas.metadata.resolution_ppi, SHEET_DPI);
        // Background, six 3×4 and their group, two 4×6 and theirs.
        let layers = &doc.canvas.layer_stack.layers;
        let groups: Vec<&str> = layers
            .iter()
            .filter(|l| l.is_group())
            .map(|l| l.name.as_str())
            .collect();
        assert_eq!(
            (layers.len(), groups),
            (11, vec!["Ảnh thẻ 3×4", "Ảnh thẻ 4×6"])
        );

        let sheet = doc.canvas.flatten_for_export();
        let at = |x: u32, y: u32| -> [u8; 3] {
            let o = ((y * pw + x) * 4) as usize;
            [sheet[o], sheet[o + 1], sheet[o + 2]]
        };
        let blocks = Sheet::Mixed.blocks(options.gap);
        let (small, large) = (&blocks[0].layout, &blocks[1].layout);
        for &(x, y) in &small.placements {
            // Blue to its very edge, white paper around, the person inside.
            assert_eq!(at(x + 1, y + 1), Backdrop::Blue.rgb());
            assert_eq!(
                at(x + small.cell_w - 2, y + small.cell_h - 2),
                Backdrop::Blue.rgb()
            );
            assert_eq!(at(x - 2, y + 30), [255; 3]);
            assert_eq!(at(x + small.cell_w / 4, y + small.cell_h / 2), [90, 80, 70]);
        }
        for &(x, y) in &large.placements {
            // White, with the cutting line just outside on all four sides.
            assert_eq!(at(x + 1, y + 1), [255; 3]);
            for (lx, ly) in [
                (x - 2, y + 30),
                (x + 30, y - 2),
                (x + large.cell_w + 1, y + 30),
                (x + 30, y + large.cell_h + 1),
            ] {
                assert_eq!(at(lx, ly), BORDER_RGB, "({lx}, {ly})");
            }
            // Two pixels of it, then paper.
            assert_eq!(at(x - 3, y + 30), [255; 3]);
            assert_eq!(at(x + large.cell_w / 4, y + large.cell_h / 2), [90, 80, 70]);
        }
        // The lower half of the sheet stays blank.
        assert_eq!(at(pw / 2, ph * 3 / 4), [255; 3]);
        assert!(
            app.shell.status_msg.contains("lệch tỷ lệ 4×6"),
            "{}",
            app.shell.status_msg
        );

        // A flat photo keeps its own background on every size.
        app.docs.active_doc_idx = 0;
        let (w, h) = PhotoKind::Id3x4.cell_px();
        let grey = [120u8, 130, 140, 255].repeat((w * h) as usize);
        app.docs.documents[0].canvas = Canvas::from_rgba(grey, w, h);
        app.do_impose_sheet(Sheet::Mixed, options);
        let doc = &app.docs.documents[app.docs.active_doc_idx];
        let sheet = doc.canvas.flatten_for_export();
        for block in Sheet::Mixed.blocks(options.gap) {
            let (x, y) = block.layout.placements[0];
            let o = (((y + 1) * doc.canvas.width + x + 1) * 4) as usize;
            assert_eq!(&sheet[o..o + 3], &[120, 130, 140]);
        }
        assert!(
            app.shell.status_msg.contains("giữ nguyên nền"),
            "{}",
            app.shell.status_msg
        );
    }

    #[test]
    fn a_sheet_of_one_size_prints_the_photo_on_the_background_it_has() {
        // Whatever "Xếp ảnh in" remembers for the mixed sheet.
        let options = SheetOptions {
            backdrop_3x4: Backdrop::White,
            backdrop_4x6: Backdrop::White,
            ..SheetOptions::default()
        };
        let blue = Backdrop::Blue.rgb();
        for (sheet, title) in [
            (
                Sheet::Grid(Paper::P10x15, PhotoKind::Id2x3),
                "Trang 10×15 — 21 tấm 2×3",
            ),
            (
                Sheet::Grid(Paper::P10x15, PhotoKind::Id4x6),
                "Trang 10×15 — 4 tấm 4×6",
            ),
        ] {
            let mut app = App::new();
            app.shell.ui.show_welcome = false;
            let mut photo = id_photo();
            let (w, h) = (photo.width, photo.height);
            photo.layer_stack.layers[0].tiles =
                TileMap::new_solid(w, h, blue[0], blue[1], blue[2], 255);
            photo.pixels_stale = true;
            app.docs.documents[0].canvas = photo;
            app.do_impose_sheet(sheet, options);

            let doc = &app.docs.documents[app.docs.active_doc_idx];
            assert_eq!(doc.title, title);
            let pixels = doc.canvas.flatten_for_export();
            let layout = &sheet.blocks(options.gap)[0].layout;
            assert_eq!(layout.placements.len(), sheet.count(options.gap));
            for &(x, y) in &layout.placements {
                let o = (((y + 1) * doc.canvas.width + x + 1) * 4) as usize;
                assert_eq!(&pixels[o..o + 3], &blue, "{title}");
            }
            assert!(
                !app.shell.status_msg.contains("giữ nguyên nền"),
                "{}",
                app.shell.status_msg
            );
        }

        // On white the photo is white, with its cutting line.
        let mut app = App::new();
        app.shell.ui.show_welcome = false;
        app.docs.documents[0].canvas = id_photo();
        let sheet = Sheet::Grid(Paper::P13x18, PhotoKind::Id3x4);
        app.do_impose_sheet(
            sheet,
            SheetOptions {
                backdrop_3x4: Backdrop::Blue,
                ..options
            },
        );
        let doc = &app.docs.documents[app.docs.active_doc_idx];
        let pixels = doc.canvas.flatten_for_export();
        let (x, y) = sheet.blocks(options.gap)[0].layout.placements[0];
        let at = |x: u32, y: u32| {
            let o = ((y * doc.canvas.width + x) * 4) as usize;
            [pixels[o], pixels[o + 1], pixels[o + 2]]
        };
        assert_eq!(at(x + 1, y + 1), [255; 3]);
        assert_eq!(at(x - 2, y + 30), BORDER_RGB);
    }
    /// Opt-in visual probe: IAI_PRINT_SHEET_PROBE is a folder of portraits;
    /// each is made an ID photo ("Làm ảnh thẻ"), laid out on the mixed sheet
    /// and saved beside it as `sheet_<name>.png` at half size.
    #[test]
    #[ignore]
    fn probe_mixed_sheets() {
        use crate::core::id_photo::{self, IdPhotoOptions};
        use crate::core::select_subject::{segment_blocking, SelectSubjectModel};
        let Ok(dir) = std::env::var("IAI_PRINT_SHEET_PROBE") else {
            return;
        };
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            if name.starts_with("sheet_") || !name.ends_with(".jpg") {
                continue;
            }
            let image = image::open(&path).unwrap().to_rgba8();
            let (w, h) = image.dimensions();
            let mut segment = |px: &[u8], sw: u32, sh: u32| {
                segment_blocking(SelectSubjectModel::BiRefNetTiny, px, sw, sh, false)
            };
            let options = IdPhotoOptions::default();
            let plan = match id_photo::prepare(
                image.as_raw(),
                w,
                h,
                None,
                &options,
                &mut segment,
                &|_| {},
            ) {
                Ok(plan) => plan,
                Err(e) => {
                    println!("{name}: {e}");
                    continue;
                }
            };
            let mut canvas = Canvas::from_rgba(image.into_raw(), w, h);
            id_photo::apply(&mut canvas, plan).unwrap();
            let mut app = App::new();
            app.shell.ui.show_welcome = false;
            app.docs.documents[0].canvas = canvas;
            app.do_impose_sheet(Sheet::Mixed, SheetOptions::default());
            println!("{name}: {}", app.shell.status_msg);
            let sheet = &app.docs.documents[app.docs.active_doc_idx].canvas;
            let full =
                image::RgbaImage::from_raw(sheet.width, sheet.height, sheet.flatten_for_export())
                    .unwrap();
            image::imageops::resize(
                &full,
                sheet.width / 2,
                sheet.height / 2,
                image::imageops::FilterType::Triangle,
            )
            .save(path.with_file_name(format!("sheet_{name}.png")))
            .unwrap();
        }
    }
}
