use super::{ExportOptions, Exporter, Importer};
use crate::core::canvas::{Canvas, CanvasMetadata, ColorSpace};
use crate::core::layer::{BlendMode, Layer, LayerType};
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};

/// Highest on-disk format version this build writes. v1 = single canvas;
/// v2 adds the multi-page PDF *project* variant (`kind == "pdf_project"`);
/// v3 adds CMYK documents (color_mode + per-layer ink planes + profile entry).
/// RGB documents keep writing v2 so older builds still open them; v3 is only
/// stamped when ink is present, which older builds MUST reject (their loader
/// errors on version > 2) rather than silently strip the ink.
///
/// v4 adds editable vector Path layers (per-layer `"path"` model payload, Bước 4).
/// Only stamped when a Path layer is present, so RGB/CMYK files without vector
/// content keep writing v2/v3 and older builds still open them; a v4 file makes
/// older builds reject (version > their max) rather than silently dropping the
/// vector model on resave.
///
/// 16-bit precision does NOT bump the version: a layer that still holds a 16-bit
/// master is written as a 16-bit RGBA PNG at `layer_{i}.png`. Older builds decode
/// it as 8-bit (graceful precision loss they couldn't use anyway); this build
/// detects the 16-bit payload on load and rebuilds the master. See
/// `docs/bit-depth-and-color-capability.md`.
const IAI_FORMAT_VERSION: u64 = 4;

pub struct IaiImporter;
pub struct IaiExporter;

/// One edited page of a saved PDF project, decoded back into a [`Canvas`].
pub struct IaiProjectPage {
    pub index: usize,
    /// The base (page 0) raster layer was byte-for-byte the imported PDF page at
    /// save time — recorded so overlay export can stay available after a reload.
    pub base_pristine: bool,
    pub view: (f32, f32, f32),
    pub canvas: Canvas,
}

/// A `.iai` PDF-project file decoded off the source: link + metadata of the
/// original PDF plus every edited page. Clean pages are re-rendered from the
/// source on demand, so only edited pages carry pixels here.
pub struct IaiPdfProject {
    pub source: PathBuf,
    pub source_len: Option<u64>,
    pub source_modified_secs: Option<u64>,
    pub embedded_pdf: Option<Vec<u8>>,
    pub page_count: usize,
    pub selected_pages: Vec<usize>,
    pub requested_dpi: f32,
    pub active_page: usize,
    pub pages: Vec<IaiProjectPage>,
}

/// Result of decoding a `.iai`: either a plain single canvas (v1 / v2 image) or
/// a multi-page PDF project (v2). The app routes each to the matching open path.
pub enum IaiLoad {
    Canvas(Canvas),
    PdfProject(IaiPdfProject),
}

fn read_manifest<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
) -> Result<serde_json::Value, String> {
    let mut f = archive
        .by_name("manifest.json")
        .map_err(|_| "Missing manifest.json")?;
    let mut s = String::new();
    f.read_to_string(&mut s).map_err(|e| e.to_string())?;
    serde_json::from_str(&s).map_err(|e| e.to_string())
}

/// Peek a `.iai` and report whether it is a multi-page PDF project (v2). Best
/// effort: any read/parse error returns `false` so the caller falls back to the
/// ordinary single-canvas open path (which surfaces a precise error).
pub fn is_pdf_project(path: &Path) -> bool {
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let Ok(mut archive) = zip::ZipArchive::new(file) else {
        return false;
    };
    read_manifest(&mut archive)
        .map(|m| m["kind"].as_str() == Some("pdf_project"))
        .unwrap_or(false)
}

/// Decode a `.iai` file into either a single canvas or a PDF project.
pub fn load(path: &Path) -> Result<IaiLoad, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
    let manifest = read_manifest(&mut archive)?;

    let version = manifest["version"].as_u64().unwrap_or(0);
    if version > IAI_FORMAT_VERSION {
        return Err(format!(
            "This file was created with a newer version of IAI (format version {}).\
             \nPlease update IAI to open it.",
            version
        ));
    }

    if manifest["kind"].as_str() == Some("pdf_project") {
        return read_pdf_project(&mut archive, &manifest).map(IaiLoad::PdfProject);
    }
    // v1 (and v2 single-image) store the canvas fields at the manifest root, with
    // layer pixels at the archive root (no prefix).
    build_canvas_from_meta(&mut archive, &manifest, "").map(IaiLoad::Canvas)
}

/// Rebuild a [`Canvas`] from its manifest metadata object and prefixed layer
/// entries. `meta` holds width/height/dpi/color_space/author/description/layers;
/// pixels live at `{prefix}layer_{i}.png` (+ optional `_mask`). Shared by the
/// single-canvas and per-page project paths.
fn build_canvas_from_meta<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
    meta: &serde_json::Value,
    prefix: &str,
) -> Result<Canvas, String> {
    let w = (meta["width"].as_u64().unwrap_or(800) as u32).max(1);
    let h = (meta["height"].as_u64().unwrap_or(600) as u32).max(1);
    let dpi = meta["dpi"].as_f64().unwrap_or(72.0) as f32;
    let color_space = match meta["color_space"].as_str().unwrap_or("sRGB") {
        "AdobeRGB" => ColorSpace::AdobeRGB,
        "ProPhoto" => ColorSpace::ProPhoto,
        _ => ColorSpace::SRGB,
    };

    let mut metadata = CanvasMetadata::default();
    metadata.resolution_ppi = dpi;
    let mut canvas = Canvas::new_blank(w, h);
    canvas.metadata = metadata;
    canvas.color_space = color_space;
    canvas.metadata.author = meta["author"].as_str().unwrap_or("").to_string();
    canvas.metadata.description = meta["description"].as_str().unwrap_or("").to_string();
    // 16-bit mode (post-B2 key; absent on older files = 8-bit). Keeps a reopened
    // 16-bit document in 16-bit mode so its first edit preserves precision
    // instead of quantizing the masters the 16-bit layer PNGs just restored.
    if meta["bit_depth"].as_u64() == Some(16) {
        canvas.bit_depth = crate::core::canvas::BitDepth::Sixteen;
    }

    canvas.layer_stack.layers.clear();

    let manifest_layers = meta["layers"].as_array();
    let declared_layer_count = meta["layer_count"]
        .as_u64()
        .unwrap_or_else(|| manifest_layers.map_or(1, |layers| layers.len() as u64))
        as usize;
    let layer_count = manifest_layers.map_or(declared_layer_count, |layers| {
        declared_layer_count.min(layers.len())
    });

    for i in 0..layer_count {
        let layer_info = &meta["layers"][i];
        let name = layer_info["name"]
            .as_str()
            .unwrap_or(&format!("Layer {}", i))
            .to_string();
        let opacity = layer_info["opacity"].as_f64().unwrap_or(1.0) as f32;
        let visible = layer_info["visible"].as_bool().unwrap_or(true);
        let locked = layer_info["locked"].as_bool().unwrap_or(false);
        let blend_mode = parse_blend_mode(layer_info["blend_mode"].as_str().unwrap_or("Normal"));
        let offset_x = layer_info["offset_x"].as_i64().unwrap_or(0) as i32;
        let offset_y = layer_info["offset_y"].as_i64().unwrap_or(0) as i32;

        let (pixels, layer_w, layer_h, hdr16) = {
            let entry_name = format!("{prefix}layer_{}.png", i);
            match archive.by_name(&entry_name) {
                Ok(mut f) => {
                    let mut buf = Vec::new();
                    f.read_to_end(&mut buf).map_err(|e| e.to_string())?;
                    let img = image::load_from_memory(&buf).map_err(|e| e.to_string())?;
                    let (lw, lh) = (img.width().max(1), img.height().max(1));
                    // A 16-bit PNG payload carries a preserved master; keep it so
                    // the layer reopens at full precision instead of being
                    // rebuilt at 8 bits from the mirror.
                    let hdr16 = matches!(
                        img,
                        image::DynamicImage::ImageRgba16(_) | image::DynamicImage::ImageRgb16(_)
                    )
                    .then(|| img.to_rgba16().into_raw());
                    (img.to_rgba8().into_raw(), lw, lh, hdr16)
                }
                Err(_) => {
                    let lw = layer_info["width"].as_u64().unwrap_or(w as u64) as u32;
                    let lh = layer_info["height"].as_u64().unwrap_or(h as u64) as u32;
                    let lw = lw.max(1);
                    let lh = lh.max(1);
                    let Some(len) = Canvas::guarded_flat_rgba_len(lw, lh) else {
                        return Err("iAi layer qua lon de tao buffer phang".to_string());
                    };
                    (vec![0u8; len], lw, lh, None)
                }
            }
        };

        let mut layer = Layer::from_rgba(i as u32, &name, pixels, layer_w, layer_h);
        // Restore the 16-bit master when the payload was 16-bit. `from_rgba`
        // built an 8-bit-only tilemap; swap in one that carries both the master
        // and its 8-bit mirror.
        if let Some(px16) = hdr16 {
            layer.tiles = crate::core::tile::TileMap::from_rgba16(&px16, layer_w, layer_h);
        }
        layer.opacity = opacity;
        layer.visible = visible;
        layer.locked = locked;
        layer.blend_mode = blend_mode;
        layer.offset = (offset_x, offset_y);
        layer.parent_id = layer_info["parent"].as_u64().map(|p| p as u32);
        layer.expanded = layer_info["expanded"].as_bool().unwrap_or(true);
        if let Some(adj) = json_to_adjustment(&layer_info["adjustment"]) {
            layer.layer_type = crate::core::layer::LayerType::Adjustment(adj);
        } else if layer_info["layer_type"].as_str() == Some("Text") {
            if let Some(text) = json_to_text_data(&layer_info["text"]) {
                layer.layer_type = crate::core::layer::LayerType::Text(text);
            }
        } else if layer_info["layer_type"].as_str() == Some("Shape") {
            if let Some(shape) = json_to_shape_data(&layer_info["shape"]) {
                layer.layer_type = crate::core::layer::LayerType::Shape(shape);
            }
        } else if layer_info["layer_type"].as_str() == Some("Path") {
            // Model is the source of truth; the baked PNG already loaded above is
            // the display fallback. A malformed/oversized payload decodes to None,
            // leaving the layer as the raster it loaded (Mục 5.3).
            if let Some(obj) = super::iai_vector::json_to_layer_path(&layer_info["path"]) {
                layer.layer_type = crate::core::layer::LayerType::Path(obj);
            }
        } else if layer_info["layer_type"].as_str() == Some("Group") {
            layer.layer_type = crate::core::layer::LayerType::Group;
        }

        let mask_entry = format!("{prefix}layer_{}_mask.png", i);
        if archive.by_name(&mask_entry).is_ok() {
            let (mask_pixels, mask_w, mask_h) = {
                let mut f = archive.by_name(&mask_entry).map_err(|e| e.to_string())?;
                let mut buf = Vec::new();
                f.read_to_end(&mut buf).map_err(|e| e.to_string())?;
                let img = image::load_from_memory(&buf).map_err(|e| e.to_string())?;
                let rgba = img.to_rgba8();
                let (mw, mh) = rgba.dimensions();
                (rgba.into_raw(), mw.max(1), mh.max(1))
            };
            use crate::core::layer::LayerMask;
            use crate::core::tile::TileMap;
            layer.mask = Some(LayerMask {
                tiles: TileMap::from_rgba(&mask_pixels, mask_w, mask_h),
                width: mask_w,
                height: mask_h,
                // Key added post-v1; older files default to enabled.
                enabled: layer_info["mask_enabled"].as_bool().unwrap_or(true),
                inverted: false,
            });
        }

        canvas.layer_stack.layers.push(layer);
    }

    if canvas.layer_stack.layers.is_empty() {
        canvas
            .layer_stack
            .layers
            .push(Layer::new(0, "Layer 1", w, h));
    }
    canvas.layer_stack.repair_next_id();

    let active_idx = meta["active_layer"].as_u64().unwrap_or(0) as usize;
    canvas.layer_stack.active_idx =
        active_idx.min(canvas.layer_stack.layers.len().saturating_sub(1));

    // CMYK document (v3): restore the conversion space and each layer's ink
    // planes. The layer PNGs already hold the profile projection (mirror), so
    // no re-projection is needed here.
    if meta["color_mode"].as_str() == Some("CMYK") {
        let profile = match meta["cmyk_profile"]["kind"].as_str() {
            Some("icc") => {
                let name = meta["cmyk_profile"]["name"]
                    .as_str()
                    .unwrap_or("CMYK profile")
                    .to_string();
                let entry = format!("{prefix}cmyk_profile.icc");
                let mut f = archive
                    .by_name(&entry)
                    .map_err(|_| "File CMYK thiếu cmyk_profile.icc".to_string())?;
                let mut data = Vec::new();
                f.read_to_end(&mut data).map_err(|e| e.to_string())?;
                crate::core::canvas::CmykProfile::Icc { name, data }
            }
            _ => crate::core::canvas::CmykProfile::Naive,
        };
        for (i, layer) in canvas.layer_stack.layers.iter_mut().enumerate() {
            let entry = format!("{prefix}layer_{}_ink.png", i);
            let Ok(mut f) = archive.by_name(&entry) else {
                continue;
            };
            let mut buf = Vec::new();
            f.read_to_end(&mut buf).map_err(|e| e.to_string())?;
            drop(f);
            let img = image::load_from_memory(&buf).map_err(|e| e.to_string())?;
            let ink = img.to_rgba8();
            let (iw, ih) = ink.dimensions();
            // The ink PNG must match the layer exactly (same writer); a
            // mismatch means corruption — leave that layer ink-less rather
            // than write rows with the wrong stride.
            if (iw, ih) == (layer.width, layer.height) {
                layer.tiles.write_ink_region(0, 0, iw, ih, &ink);
            }
        }
        canvas.color_mode = crate::core::canvas::ColorMode::Cmyk(profile);
    }

    // Saved alpha channels (post-v2 addition; missing key/entries = none).
    if let Some(alpha_list) = meta["alpha_channels"].as_array() {
        for (i, info) in alpha_list.iter().enumerate() {
            let entry_name = format!("{prefix}channel_{}.png", i);
            let Ok(mut f) = archive.by_name(&entry_name) else {
                continue;
            };
            let mut buf = Vec::new();
            if f.read_to_end(&mut buf).is_err() {
                continue;
            }
            drop(f);
            let Ok(img) = image::load_from_memory(&buf) else {
                continue;
            };
            let gray = img.to_luma8();
            let (cw, ch) = gray.dimensions();
            if cw == 0 || ch == 0 {
                continue;
            }
            let name = info["name"].as_str().unwrap_or("").to_string();
            canvas
                .channels
                .add_alpha(name, gray.into_raw(), cw.max(1), ch.max(1));
        }
    }

    canvas.flatten_full();
    Ok(canvas)
}

fn read_pdf_project<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
    manifest: &serde_json::Value,
) -> Result<IaiPdfProject, String> {
    let project = &manifest["pdf_project"];
    let source = PathBuf::from(
        project["source"]
            .as_str()
            .ok_or("PDF project is missing its source path")?,
    );
    let page_count = project["page_count"].as_u64().unwrap_or(0) as usize;
    let requested_dpi = project["requested_dpi"].as_f64().unwrap_or(300.0) as f32;
    let active_page = project["active_page"].as_u64().unwrap_or(0) as usize;
    let source_len = project["source_len"].as_u64();
    let source_modified_secs = project["source_modified"].as_u64();
    let selected_pages: Vec<usize> = project["selected_pages"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_u64())
                .map(|v| v as usize)
                .collect()
        })
        .unwrap_or_else(|| (0..page_count).collect());

    let mut pages = Vec::new();
    if let Some(arr) = manifest["pages"].as_array() {
        for entry in arr {
            let index = entry["index"].as_u64().unwrap_or(0) as usize;
            let base_pristine = entry["base_pristine"].as_bool().unwrap_or(false);
            let view = {
                let v = &entry["view"];
                (
                    v.get(0).and_then(|x| x.as_f64()).unwrap_or(0.0) as f32,
                    v.get(1).and_then(|x| x.as_f64()).unwrap_or(0.0) as f32,
                    v.get(2).and_then(|x| x.as_f64()).unwrap_or(0.0) as f32,
                )
            };
            let prefix = format!("page_{index}/");
            let canvas = build_canvas_from_meta(archive, entry, &prefix)?;
            pages.push(IaiProjectPage {
                index,
                base_pristine,
                view,
                canvas,
            });
        }
    }

    let embedded_pdf = match archive.by_name("source.pdf") {
        Ok(mut f) => {
            let mut buf = Vec::new();
            f.read_to_end(&mut buf).map_err(|e| e.to_string())?;
            Some(buf)
        }
        Err(_) => None,
    };

    Ok(IaiPdfProject {
        source,
        source_len,
        source_modified_secs,
        embedded_pdf,
        page_count,
        selected_pages,
        requested_dpi,
        active_page,
        pages,
    })
}

impl Importer for IaiImporter {
    fn extensions(&self) -> &[&str] {
        &["iai"]
    }

    fn import(&self, path: &Path) -> Result<Canvas, String> {
        match load(path)? {
            IaiLoad::Canvas(canvas) => Ok(canvas),
            // A project file opened through the plain image path collapses to its
            // active page; the app routes projects through their own open path.
            IaiLoad::PdfProject(project) => {
                let active = project.active_page;
                let mut pages = project.pages;
                let idx = pages
                    .iter()
                    .position(|p| p.index == active)
                    .or(if pages.is_empty() { None } else { Some(0) })
                    .ok_or_else(|| "PDF project has no stored pages to display".to_string())?;
                Ok(pages.swap_remove(idx).canvas)
            }
        }
    }
}

impl Exporter for IaiExporter {
    fn extensions(&self) -> &[&str] {
        &["iai"]
    }

    fn export(&self, canvas: &Canvas, path: &Path, _opts: &ExportOptions) -> Result<(), String> {
        write_iai_archive(path, |zip| {
            let options = deflated_options();
            let mut manifest = canvas_meta_json(canvas);
            // Graduated version so files stay openable by older builds when they
            // can be: a Path layer forces v4 (vector model would be lost on an
            // older resave), else CMYK stamps v3 (ink), else RGB stays v2.
            let has_path = canvas
                .layer_stack
                .layers
                .iter()
                .any(|l| matches!(l.layer_type, crate::core::layer::LayerType::Path(_)));
            manifest["version"] = serde_json::json!(if has_path {
                4u64
            } else if canvas.is_cmyk() {
                3u64
            } else {
                2u64
            });

            zip.start_file("manifest.json", options)
                .map_err(|e| e.to_string())?;
            zip.write_all(manifest.to_string().as_bytes())
                .map_err(|e| e.to_string())?;

            write_thumbnail(zip, canvas)?;
            write_canvas_layers(zip, canvas, "")
        })
    }
}

fn deflated_options() -> zip::write::SimpleFileOptions {
    zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated)
}

fn stored_options() -> zip::write::SimpleFileOptions {
    zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored)
}

/// Open a `.iai` temp file, run `body` against the zip writer, then finish and
/// atomically rename into place. A failure removes the temp file so a partial
/// write never replaces the user's project.
fn write_iai_archive(
    path: &Path,
    body: impl FnOnce(&mut zip::ZipWriter<std::fs::File>) -> Result<(), String>,
) -> Result<(), String> {
    let tmp_path = path.with_extension("iai.tmp");
    let result = (|| {
        let file = std::fs::File::create(&tmp_path).map_err(|e| e.to_string())?;
        let mut zip = zip::ZipWriter::new(file);
        body(&mut zip)?;
        zip.finish().map_err(|e| e.to_string())?;
        Ok(())
    })();
    match result {
        Ok(()) => std::fs::rename(&tmp_path, path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp_path);
            e.to_string()
        }),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp_path);
            Err(e)
        }
    }
}

/// The per-canvas metadata object (width/height/dpi/color_space/author/
/// description/layers). Used both at the manifest root (single canvas) and per
/// page of a PDF project.
fn canvas_meta_json(canvas: &Canvas) -> serde_json::Value {
    let mut layers_json: Vec<serde_json::Value> =
        Vec::with_capacity(canvas.layer_stack.layers.len());
    for layer in canvas.layer_stack.layers.iter() {
        let parent_index: serde_json::Value = layer
            .parent_id
            .and_then(|pid| canvas.layer_stack.layers.iter().position(|l| l.id == pid))
            .map(|p| serde_json::json!(p as u64))
            .unwrap_or(serde_json::Value::Null);
        let adj_json = if let crate::core::layer::LayerType::Adjustment(ref adj) = layer.layer_type
        {
            adjustment_to_json(adj)
        } else {
            serde_json::Value::Null
        };
        let text_json = if let crate::core::layer::LayerType::Text(ref text) = layer.layer_type {
            text_to_json(text)
        } else {
            serde_json::Value::Null
        };
        let shape_json = if let crate::core::layer::LayerType::Shape(ref shape) = layer.layer_type {
            shape_to_json(shape)
        } else {
            serde_json::Value::Null
        };
        let path_json = if let crate::core::layer::LayerType::Path(ref obj) = layer.layer_type {
            super::iai_vector::layer_path_to_json(obj)
        } else {
            serde_json::Value::Null
        };
        layers_json.push(serde_json::json!({
            "name": layer.name,
            "opacity": layer.opacity,
            "visible": layer.visible,
            "locked": layer.locked,
            "width": layer.width,
            "height": layer.height,
            "blend_mode": blend_mode_to_str(layer.blend_mode),
            "has_mask": layer.mask.is_some(),
            "mask_enabled": layer.mask.as_ref().map_or(true, |m| m.enabled),
            "layer_type": layer_type_to_str(&layer.layer_type),
            "offset_x": layer.offset.0,
            "offset_y": layer.offset.1,
            "parent": parent_index,
            "expanded": layer.expanded,
            "adjustment": adj_json,
            "text": text_json,
            "shape": shape_json,
            "path": path_json,
        }));
    }

    // Saved alpha channels (Channels panel). Pixels live at
    // {prefix}channel_{i}.png; older builds ignore both the key and the
    // entries (resaving there drops the channels — accepted, no version bump).
    let alpha_json: Vec<serde_json::Value> = canvas
        .channels
        .alpha
        .iter()
        .map(|ch| {
            serde_json::json!({
                "name": ch.name,
                "width": ch.width,
                "height": ch.height,
            })
        })
        .collect();

    // Document bit-depth mode. Persisted so a reopened 16-bit document stays in
    // 16-bit mode and edits keep preserving the masters that the 16-bit layer
    // PNGs restore. Older builds ignore this key (they default to 8-bit).
    let bit_depth = if canvas.bit_depth == crate::core::canvas::BitDepth::Sixteen {
        16
    } else {
        8
    };
    let mut meta = serde_json::json!({
        "width": canvas.width,
        "height": canvas.height,
        "dpi": canvas.metadata.resolution_ppi,
        "color_space": color_space_to_str(canvas.color_space),
        "bit_depth": bit_depth,
        "author": canvas.metadata.author,
        "description": canvas.metadata.description,
        "layer_count": canvas.layer_stack.layers.len(),
        "active_layer": canvas.layer_stack.active_idx,
        "layers": layers_json,
        "alpha_channels": alpha_json,
    });

    // CMYK (v3): mode tag + conversion space. Ink pixels live at
    // {prefix}layer_{i}_ink.png; an ICC space's bytes at {prefix}cmyk_profile.icc.
    if let crate::core::canvas::ColorMode::Cmyk(ref profile) = canvas.color_mode {
        meta["color_mode"] = serde_json::json!("CMYK");
        meta["cmyk_profile"] = match profile {
            crate::core::canvas::CmykProfile::Naive => serde_json::json!({"kind": "naive"}),
            crate::core::canvas::CmykProfile::Icc { name, .. } => {
                serde_json::json!({"kind": "icc", "name": name})
            }
        };
    }
    meta
}

fn write_thumbnail(zip: &mut zip::ZipWriter<std::fs::File>, canvas: &Canvas) -> Result<(), String> {
    let flat_preview = canvas.export_flat();
    let (thumb, tw, th) = make_thumbnail(&flat_preview, canvas.width, canvas.height, 256);
    let thumb_png = encode_png(&thumb, tw, th)?;
    zip.start_file("thumbnail.png", stored_options())
        .map_err(|e| e.to_string())?;
    zip.write_all(&thumb_png).map_err(|e| e.to_string())
}

/// Write a canvas's layer (+ mask) PNGs under `{prefix}layer_{i}.png`.
fn write_canvas_layers(
    zip: &mut zip::ZipWriter<std::fs::File>,
    canvas: &Canvas,
    prefix: &str,
) -> Result<(), String> {
    let stored = stored_options();
    for (i, layer) in canvas.layer_stack.layers.iter().enumerate() {
        // A layer still carrying a full 16-bit master (RAW / 16-bit import,
        // untouched by 8-bit edits) is written as a 16-bit RGBA PNG so precision
        // round-trips. No format-version bump is needed: a 16-bit PNG decodes
        // fine as 8-bit, so older builds still open the file (they just can't
        // use the extra precision). Layers without a master stay 8-bit.
        let png_data = if layer.tiles.has_hdr() {
            encode_png16(&layer.tiles.flatten16(), layer.width, layer.height)?
        } else {
            encode_png(&layer.flatten_tiles(), layer.width, layer.height)?
        };
        zip.start_file(format!("{prefix}layer_{}.png", i), stored)
            .map_err(|e| e.to_string())?;
        zip.write_all(&png_data).map_err(|e| e.to_string())?;

        if let Some(ref mask) = layer.mask {
            let flat_mask = mask.tiles.flatten();
            let mut gray_mask = Vec::with_capacity((mask.width as usize) * (mask.height as usize));
            for px in (0..flat_mask.len()).step_by(4) {
                gray_mask.push(flat_mask[px]);
            }
            let mask_png = encode_gray_png(&gray_mask, mask.width, mask.height)?;
            zip.start_file(format!("{prefix}layer_{}_mask.png", i), stored)
                .map_err(|e| e.to_string())?;
            zip.write_all(&mask_png).map_err(|e| e.to_string())?;
        }
    }

    // Saved alpha channels (Channels panel), indexed to match the
    // "alpha_channels" manifest array.
    for (i, ch) in canvas.channels.alpha.iter().enumerate() {
        let png = encode_gray_png(&ch.mask, ch.width, ch.height)?;
        zip.start_file(format!("{prefix}channel_{}.png", i), stored)
            .map_err(|e| e.to_string())?;
        zip.write_all(&png).map_err(|e| e.to_string())?;
    }

    // CMYK (v3): each layer's ink planes ride in an RGBA8 PNG carrying C,M,Y,K
    // in the R,G,B,A slots; the ICC space (when not naive) is stored once.
    if let crate::core::canvas::ColorMode::Cmyk(ref profile) = canvas.color_mode {
        for (i, layer) in canvas.layer_stack.layers.iter().enumerate() {
            let (lw, lh) = (layer.width, layer.height);
            let Some(len) = Canvas::checked_rgba_len(lw, lh) else {
                return Err("Layer CMYK quá lớn để lưu ink".to_string());
            };
            let mut ink = vec![0u8; len];
            layer.tiles.extract_ink_region_into(0, 0, lw, lh, &mut ink);
            let png = encode_png(&ink, lw, lh)?;
            zip.start_file(format!("{prefix}layer_{}_ink.png", i), stored)
                .map_err(|e| e.to_string())?;
            zip.write_all(&png).map_err(|e| e.to_string())?;
        }
        if let crate::core::canvas::CmykProfile::Icc { data, .. } = profile {
            zip.start_file(format!("{prefix}cmyk_profile.icc"), stored)
                .map_err(|e| e.to_string())?;
            zip.write_all(data).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// One edited page to write into a project archive: its canvas, page index,
/// whether the base layer is still the pristine PDF render, and its saved view.
pub struct IaiProjectPageOut<'a> {
    pub index: usize,
    pub base_pristine: bool,
    pub view: (f32, f32, f32),
    pub canvas: &'a Canvas,
}

/// Project-level link + metadata written alongside the edited pages.
pub struct IaiProjectMeta {
    pub source: PathBuf,
    pub source_len: Option<u64>,
    pub source_modified_secs: Option<u64>,
    pub page_count: usize,
    pub selected_pages: Vec<usize>,
    pub requested_dpi: f32,
    pub active_page: usize,
}

/// Write a multi-page PDF project `.iai` (format v2) atomically. Stores the link
/// to the original PDF plus every edited page's layers; clean pages are omitted
/// (re-rendered from the source on open).
pub fn save_pdf_project(
    path: &Path,
    meta: &IaiProjectMeta,
    pages: &[IaiProjectPageOut],
    source_pdf: Option<&[u8]>,
) -> Result<(), String> {
    write_iai_archive(path, |zip| {
        let pages_json: Vec<serde_json::Value> = pages
            .iter()
            .map(|page| {
                let mut entry = canvas_meta_json(page.canvas);
                entry["index"] = serde_json::json!(page.index);
                entry["base_pristine"] = serde_json::json!(page.base_pristine);
                entry["view"] = serde_json::json!([page.view.0, page.view.1, page.view.2]);
                entry
            })
            .collect();

        // Graduated version (mirrors the single-canvas path): a Path layer on any
        // page forces v4, else CMYK ink needs v3, else stay v2 so older builds
        // keep opening the project.
        let has_path = pages.iter().any(|p| {
            p.canvas
                .layer_stack
                .layers
                .iter()
                .any(|l| matches!(l.layer_type, crate::core::layer::LayerType::Path(_)))
        });
        let version = if has_path {
            4u64
        } else if pages.iter().any(|p| p.canvas.is_cmyk()) {
            3u64
        } else {
            2u64
        };
        let manifest = serde_json::json!({
            "version": version,
            "kind": "pdf_project",
            "pdf_project": {
                "source": meta.source.to_string_lossy(),
                "source_len": meta.source_len,
                "source_modified": meta.source_modified_secs,
                "page_count": meta.page_count,
                "selected_pages": meta.selected_pages,
                "requested_dpi": meta.requested_dpi,
                "active_page": meta.active_page,
                "embedded": source_pdf.is_some(),
            },
            "pages": pages_json,
        });

        zip.start_file("manifest.json", deflated_options())
            .map_err(|e| e.to_string())?;
        zip.write_all(manifest.to_string().as_bytes())
            .map_err(|e| e.to_string())?;

        if let Some(bytes) = source_pdf {
            zip.start_file("source.pdf", stored_options())
                .map_err(|e| e.to_string())?;
            zip.write_all(bytes).map_err(|e| e.to_string())?;
        }

        // Thumbnail from the active page when it is stored, else the first page.
        if let Some(page) = pages
            .iter()
            .find(|p| p.index == meta.active_page)
            .or_else(|| pages.first())
        {
            write_thumbnail(zip, page.canvas)?;
        }

        for page in pages {
            let prefix = format!("page_{}/", page.index);
            write_canvas_layers(zip, page.canvas, &prefix)?;
        }
        Ok(())
    })
}

fn encode_png(pixels: &[u8], w: u32, h: u32) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut buf, w, h);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        writer.write_image_data(pixels).map_err(|e| e.to_string())?;
    }
    Ok(buf)
}

/// Encode 16-bit RGBA as a PNG, preserving a layer's 16-bit master so `.iai`
/// round-trips precision. PNG stores 16-bit samples big-endian.
fn encode_png16(px16: &[u16], w: u32, h: u32) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut buf, w, h);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Sixteen);
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        let mut be = Vec::with_capacity(px16.len() * 2);
        for &s in px16 {
            be.extend_from_slice(&s.to_be_bytes());
        }
        writer.write_image_data(&be).map_err(|e| e.to_string())?;
    }
    Ok(buf)
}

fn encode_gray_png(pixels: &[u8], w: u32, h: u32) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut buf, w, h);
        encoder.set_color(png::ColorType::Grayscale);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        writer.write_image_data(pixels).map_err(|e| e.to_string())?;
    }
    Ok(buf)
}

fn make_thumbnail(pixels: &[u8], w: u32, h: u32, size: u32) -> (Vec<u8>, u32, u32) {
    if pixels.is_empty() {
        return (vec![255u8; (size * size * 4) as usize], size, size);
    }
    let scale = (size as f32 / w as f32)
        .min(size as f32 / h as f32)
        .min(1.0);
    let tw = (w as f32 * scale) as u32;
    let th = (h as f32 * scale) as u32;
    if tw == 0 || th == 0 {
        return (vec![255u8; (size * size * 4) as usize], size, size);
    }

    let mut thumb = vec![0u8; (tw * th * 4) as usize];
    for y in 0..th {
        for x in 0..tw {
            let sx = (x as f32 / scale) as u32;
            let sy = (y as f32 / scale) as u32;
            let si = ((sy * w + sx) * 4) as usize;
            let di = ((y * tw + x) * 4) as usize;
            if si + 3 < pixels.len() && di + 3 < thumb.len() {
                thumb[di..di + 4].copy_from_slice(&pixels[si..si + 4]);
            }
        }
    }
    (thumb, tw, th)
}

fn parse_blend_mode(s: &str) -> BlendMode {
    match s {
        "Multiply" => BlendMode::Multiply,
        "Screen" => BlendMode::Screen,
        "Overlay" => BlendMode::Overlay,
        "Darken" => BlendMode::Darken,
        "Lighten" => BlendMode::Lighten,
        "ColorDodge" => BlendMode::ColorDodge,
        "ColorBurn" => BlendMode::ColorBurn,
        "HardLight" => BlendMode::HardLight,
        "SoftLight" => BlendMode::SoftLight,
        "LinearLight" => BlendMode::LinearLight,
        "Difference" => BlendMode::Difference,
        "Exclusion" => BlendMode::Exclusion,
        "Hue" => BlendMode::Hue,
        "Saturation" => BlendMode::Saturation,
        "Color" => BlendMode::Color,
        "Luminosity" => BlendMode::Luminosity,
        _ => BlendMode::Normal,
    }
}

fn blend_mode_to_str(mode: BlendMode) -> &'static str {
    match mode {
        BlendMode::Normal => "Normal",
        BlendMode::Dissolve => "Dissolve",
        BlendMode::Multiply => "Multiply",
        BlendMode::Screen => "Screen",
        BlendMode::Overlay => "Overlay",
        BlendMode::Darken => "Darken",
        BlendMode::Lighten => "Lighten",
        BlendMode::ColorDodge => "ColorDodge",
        BlendMode::ColorBurn => "ColorBurn",
        BlendMode::HardLight => "HardLight",
        BlendMode::SoftLight => "SoftLight",
        BlendMode::LinearLight => "LinearLight",
        BlendMode::Difference => "Difference",
        BlendMode::Exclusion => "Exclusion",
        BlendMode::Hue => "Hue",
        BlendMode::Saturation => "Saturation",
        BlendMode::Color => "Color",
        BlendMode::Luminosity => "Luminosity",
    }
}

fn layer_type_to_str(lt: &LayerType) -> &'static str {
    match lt {
        LayerType::Raster => "Raster",
        LayerType::Adjustment(_) => "Adjustment",
        LayerType::Group => "Group",
        LayerType::Text(_) => "Text",
        LayerType::Shape(_) => "Shape",
        LayerType::Path(_) => "Path",
        LayerType::SmartObject => "SmartObject",
    }
}

fn color_space_to_str(cs: crate::core::canvas::ColorSpace) -> &'static str {
    match cs {
        crate::core::canvas::ColorSpace::SRGB => "sRGB",
        crate::core::canvas::ColorSpace::AdobeRGB => "AdobeRGB",
        crate::core::canvas::ColorSpace::ProPhoto => "ProPhoto",
        crate::core::canvas::ColorSpace::LinearRGB => "LinearRGB",
    }
}

fn text_to_json(td: &crate::core::text::TextData) -> serde_json::Value {
    let mut obj = serde_json::json!({
        "content": &td.content,
        "font_px": td.font_px,
        "color": td.color,
        "font_family": td.font_family.storage_name(),
        "align": text_align_to_str(td.align),
        "line_height": td.line_height,
        "bold": td.bold,
        "italic": td.italic,
        "underline": td.underline,
        "tracking_px": td.tracking_px,
        "opacity": td.opacity,
        "stretch_x": td.stretch_x,
        "rotation_deg": td.rotation_deg,
        "flip_x": td.flip_x,
        "flip_y": td.flip_y,
    });
    // Per-character styles, run-length encoded to stay compact for uniform
    // text (the common case serializes nothing).
    if !td.glyph_styles.is_empty() {
        let mut runs = Vec::new();
        let mut i = 0usize;
        while i < td.glyph_styles.len() {
            let s = td.glyph_styles[i].clone();
            let mut j = i + 1;
            while j < td.glyph_styles.len()
                && td.glyph_styles.get(j).is_some_and(|style| style == &s)
            {
                j += 1;
            }
            runs.push(serde_json::json!({
                "n": j - i,
                "color": s.color,
                "font_px": s.font_px,
                "font_family": s.font_family.storage_name(),
                "bold": s.bold,
                "italic": s.italic,
                "underline": s.underline,
            }));
            i = j;
        }
        obj["glyph_runs"] = serde_json::Value::Array(runs);
    }
    obj
}

fn shape_to_json(sd: &crate::core::shape::ShapeData) -> serde_json::Value {
    serde_json::json!({
        "kind": sd.kind.to_u8(),
        "x0": sd.x0,
        "y0": sd.y0,
        "x1": sd.x1,
        "y1": sd.y1,
        "corner_radius": sd.corner_radius,
        "fill": sd.fill,
        "fill_color": sd.fill_color,
        "stroke_width": sd.stroke_width,
        "stroke_color": sd.stroke_color,
    })
}

fn json_to_shape_data(v: &serde_json::Value) -> Option<crate::core::shape::ShapeData> {
    use crate::core::shape::{ShapeData, ShapeKind};
    if !v.is_object() {
        return None;
    }
    let f = |k: &str| v[k].as_f64().map(|n| n as f32);
    let color = |k: &str| -> Option<[u8; 4]> {
        let a = v[k].as_array()?;
        if a.len() != 4 {
            return None;
        }
        Some([
            a[0].as_u64()? as u8,
            a[1].as_u64()? as u8,
            a[2].as_u64()? as u8,
            a[3].as_u64()? as u8,
        ])
    };
    Some(ShapeData {
        kind: ShapeKind::from_u8(v["kind"].as_u64().unwrap_or(0) as u8),
        x0: f("x0")?,
        y0: f("y0")?,
        x1: f("x1")?,
        y1: f("y1")?,
        corner_radius: f("corner_radius").unwrap_or(0.0),
        fill: v["fill"].as_bool().unwrap_or(true),
        fill_color: color("fill_color").unwrap_or([0, 0, 0, 255]),
        stroke_width: f("stroke_width").unwrap_or(0.0),
        stroke_color: color("stroke_color").unwrap_or([0, 0, 0, 255]),
    })
}

fn json_to_text_data(v: &serde_json::Value) -> Option<crate::core::text::TextData> {
    if !v.is_object() {
        return None;
    }
    let content = v["content"].as_str()?.to_string();
    let font_px = v["font_px"].as_f64().unwrap_or(48.0) as f32;
    let color = json_to_rgba(&v["color"]).unwrap_or([0, 0, 0, 255]);
    let font_family = crate::core::text::TextFontFamily::from_storage_name(
        v["font_family"].as_str().unwrap_or("SegoeUi"),
    );
    let align = json_to_text_align(v["align"].as_str().unwrap_or("Left"));
    let line_height = v["line_height"].as_f64().unwrap_or(1.2) as f32;
    let bold = v["bold"].as_bool().unwrap_or(false);
    let italic = v["italic"].as_bool().unwrap_or(false);
    let underline = v["underline"].as_bool().unwrap_or(false);
    let tracking_px = v["tracking_px"].as_f64().unwrap_or(0.0) as f32;
    let opacity = v["opacity"].as_f64().unwrap_or(1.0) as f32;
    let stretch_x = v["stretch_x"].as_f64().unwrap_or(1.0) as f32;
    let rotation_deg = v["rotation_deg"].as_f64().unwrap_or(0.0) as f32;
    let flip_x = v["flip_x"].as_bool().unwrap_or(false);
    let flip_y = v["flip_y"].as_bool().unwrap_or(false);
    let mut glyph_styles = Vec::new();
    if let Some(runs) = v["glyph_runs"].as_array() {
        for run in runs {
            let n = run["n"].as_u64().unwrap_or(0) as usize;
            let rc = json_to_rgba(&run["color"]).unwrap_or(color);
            let rpx = run["font_px"].as_f64().unwrap_or(font_px as f64) as f32;
            let rfamily = run["font_family"]
                .as_str()
                .map(crate::core::text::TextFontFamily::from_storage_name)
                .unwrap_or_else(|| font_family.clone());
            let rbold = run["bold"].as_bool().unwrap_or(bold);
            let ritalic = run["italic"].as_bool().unwrap_or(italic);
            let runderline = run["underline"].as_bool().unwrap_or(underline);
            for _ in 0..n {
                glyph_styles.push(crate::core::text::GlyphStyle {
                    color: rc,
                    font_px: rpx,
                    font_family: rfamily.clone(),
                    bold: rbold,
                    italic: ritalic,
                    underline: runderline,
                });
            }
        }
        // Guard against a corrupt run table that doesn't match the content.
        if glyph_styles.len() != content.chars().count() {
            glyph_styles.clear();
        }
    }
    Some(crate::core::text::TextData {
        content,
        font_px,
        color,
        font_family,
        align,
        line_height,
        bold,
        italic,
        underline,
        tracking_px,
        opacity,
        stretch_x,
        rotation_deg,
        flip_x,
        flip_y,
        glyph_styles,
    })
}

fn text_align_to_str(align: crate::core::text::TextAlign) -> &'static str {
    match align {
        crate::core::text::TextAlign::Left => "Left",
        crate::core::text::TextAlign::Center => "Center",
        crate::core::text::TextAlign::Right => "Right",
    }
}

fn json_to_text_align(s: &str) -> crate::core::text::TextAlign {
    match s {
        "Center" => crate::core::text::TextAlign::Center,
        "Right" => crate::core::text::TextAlign::Right,
        _ => crate::core::text::TextAlign::Left,
    }
}

fn json_to_rgba(v: &serde_json::Value) -> Option<[u8; 4]> {
    let arr = v.as_array()?;
    if arr.len() != 4 {
        return None;
    }
    let mut out = [0u8; 4];
    for (i, channel) in arr.iter().enumerate() {
        out[i] = channel.as_u64().unwrap_or(0).min(255) as u8;
    }
    Some(out)
}

fn adjustment_to_json(adj: &crate::core::layer::AdjustmentType) -> serde_json::Value {
    use crate::core::layer::AdjustmentType as A;
    match adj {
        A::BrightnessContrast {
            brightness,
            contrast,
        } => serde_json::json!({
            "type": "BrightnessContrast", "brightness": brightness, "contrast": contrast
        }),
        A::HueSaturation {
            hue,
            saturation,
            lightness,
        } => serde_json::json!({
            "type": "HueSaturation", "hue": hue, "saturation": saturation, "lightness": lightness
        }),
        A::Levels { channels } => {
            // Legacy keys carry the master channel so v≤2 builds keep reading
            // these files; r/g/b are written only when non-identity.
            let ch_json = |ch: &crate::core::layer::LevelsParams| {
                serde_json::json!({
                    "in_black": ch.in_black, "in_white": ch.in_white, "gamma": ch.gamma,
                    "out_black": ch.out_black, "out_white": ch.out_white
                })
            };
            let mut obj = serde_json::json!({
                "type": "Levels",
                "in_black": channels[0].in_black, "in_white": channels[0].in_white,
                "gamma": channels[0].gamma,
                "out_black": channels[0].out_black, "out_white": channels[0].out_white
            });
            for (key, ch) in [
                ("r", &channels[1]),
                ("g", &channels[2]),
                ("b", &channels[3]),
            ] {
                if !ch.is_identity() {
                    obj[key] = ch_json(ch);
                }
            }
            obj
        }
        A::Curves { channels } => {
            // Legacy "points" carries the master curve; per-channel curves are
            // written only when non-identity.
            let pts_json = |pts: &[(f32, f32)]| {
                serde_json::json!(pts.iter().map(|(x, y)| [x, y]).collect::<Vec<_>>())
            };
            let mut obj = serde_json::json!({
                "type": "Curves",
                "points": pts_json(&channels[0])
            });
            for (key, ch) in [
                ("points_r", &channels[1]),
                ("points_g", &channels[2]),
                ("points_b", &channels[3]),
            ] {
                if !crate::core::layer::curve_is_identity(ch) {
                    obj[key] = pts_json(ch);
                }
            }
            obj
        }
        A::ColorBalance {
            shadows,
            midtones,
            highlights,
            preserve_luminosity,
        } => serde_json::json!({
            "type": "ColorBalance",
            "shadows": shadows, "midtones": midtones, "highlights": highlights,
            "preserve_luminosity": preserve_luminosity
        }),
        A::Vibrance {
            vibrance,
            saturation,
        } => serde_json::json!({
            "type": "Vibrance", "vibrance": vibrance, "saturation": saturation
        }),
        A::Exposure {
            exposure,
            offset,
            gamma,
        } => serde_json::json!({
            "type": "Exposure", "exposure": exposure, "offset": offset, "gamma": gamma
        }),
        A::Invert => serde_json::json!({ "type": "Invert" }),
        A::Threshold { value } => serde_json::json!({ "type": "Threshold", "value": value }),
        A::Posterize { levels } => serde_json::json!({ "type": "Posterize", "levels": levels }),
        A::BlackAndWhite { r, y, g, c, b, m } => serde_json::json!({
            "type": "BlackAndWhite", "r": r, "y": y, "g": g, "c": c, "b": b, "m": m
        }),
        A::PhotoFilter {
            color,
            density,
            luminosity,
        } => serde_json::json!({
            "type": "PhotoFilter",
            "color": color, "density": density, "luminosity": luminosity
        }),
        A::GradientMap {
            stops,
            reverse,
            dither,
        } => {
            let stops_json: Vec<serde_json::Value> = stops
                .iter()
                .map(|(p, c)| serde_json::json!({ "pos": p, "color": [c[0], c[1], c[2]] }))
                .collect();
            serde_json::json!({
                "type": "GradientMap",
                "stops": stops_json,
                "reverse": reverse,
                "dither": dither
            })
        }
        A::Desaturate => serde_json::json!({ "type": "Desaturate" }),
        A::ChannelMixer {
            red,
            green,
            blue,
            monochrome,
        } => serde_json::json!({
            "type": "ChannelMixer",
            "red": red, "green": green, "blue": blue, "monochrome": monochrome
        }),
    }
}

fn json_to_adjustment(v: &serde_json::Value) -> Option<crate::core::layer::AdjustmentType> {
    use crate::core::layer::AdjustmentType as A;
    let t = v["type"].as_str()?;
    let f32v = |key: &str| v[key].as_f64().unwrap_or(0.0) as f32;
    let u8v = |key: &str| v[key].as_u64().unwrap_or(0) as u8;
    let boolv = |key: &str| v[key].as_bool().unwrap_or(false);
    let arr3 = |key: &str| -> [f32; 3] {
        let a = &v[key];
        let g = |i: usize| a.get(i).and_then(|x| x.as_f64()).unwrap_or(0.0) as f32;
        [g(0), g(1), g(2)]
    };
    Some(match t {
        "BrightnessContrast" => A::BrightnessContrast {
            brightness: f32v("brightness"),
            contrast: f32v("contrast"),
        },
        "HueSaturation" => A::HueSaturation {
            hue: f32v("hue"),
            saturation: f32v("saturation"),
            lightness: f32v("lightness"),
        },
        "Levels" => {
            use crate::core::layer::LevelsParams;
            let parse_ch = |val: &serde_json::Value| LevelsParams {
                in_black: val["in_black"].as_u64().unwrap_or(0) as u8,
                in_white: val["in_white"].as_u64().unwrap_or(255) as u8,
                gamma: val["gamma"].as_f64().unwrap_or(1.0) as f32,
                out_black: val["out_black"].as_u64().unwrap_or(0) as u8,
                out_white: val["out_white"].as_u64().unwrap_or(255) as u8,
            };
            // Legacy keys are the master channel; "r"/"g"/"b" objects are
            // optional (absent = identity, incl. every v≤2 file).
            let mut channels = [LevelsParams::default(); 4];
            channels[0] = parse_ch(v);
            for (i, key) in [(1usize, "r"), (2, "g"), (3, "b")] {
                if v[key].is_object() {
                    channels[i] = parse_ch(&v[key]);
                }
            }
            A::Levels { channels }
        }
        "Curves" => {
            let parse_pts = |val: &serde_json::Value| -> Option<Vec<(f32, f32)>> {
                val.as_array().map(|arr| {
                    arr.iter()
                        .filter_map(|p| {
                            Some((p.get(0)?.as_f64()? as f32, p.get(1)?.as_f64()? as f32))
                        })
                        .collect()
                })
            };
            // Legacy "points" is the master curve; "points_r/g/b" are optional
            // (absent = identity, incl. every v≤2 file).
            let mut channels: [Vec<(f32, f32)>; 4] =
                std::array::from_fn(|_| crate::core::layer::identity_curve());
            channels[0] = parse_pts(&v["points"]).unwrap_or_default();
            for (i, key) in [(1usize, "points_r"), (2, "points_g"), (3, "points_b")] {
                if let Some(pts) = parse_pts(&v[key]) {
                    channels[i] = pts;
                }
            }
            A::Curves { channels }
        }
        "ColorBalance" => A::ColorBalance {
            shadows: arr3("shadows"),
            midtones: arr3("midtones"),
            highlights: arr3("highlights"),
            preserve_luminosity: boolv("preserve_luminosity"),
        },
        "Vibrance" => A::Vibrance {
            vibrance: f32v("vibrance"),
            saturation: f32v("saturation"),
        },
        "Exposure" => A::Exposure {
            exposure: f32v("exposure"),
            offset: f32v("offset"),
            gamma: f32v("gamma"),
        },
        "Invert" => A::Invert,
        "Threshold" => A::Threshold {
            value: u8v("value"),
        },
        "Posterize" => A::Posterize {
            levels: u8v("levels"),
        },
        "BlackAndWhite" => A::BlackAndWhite {
            r: f32v("r"),
            y: f32v("y"),
            g: f32v("g"),
            c: f32v("c"),
            b: f32v("b"),
            m: f32v("m"),
        },
        "PhotoFilter" => {
            let color_arr = &v["color"];
            let cc = |i: usize| color_arr.get(i).and_then(|x| x.as_u64()).unwrap_or(0) as u8;
            let color = [cc(0), cc(1), cc(2)];
            A::PhotoFilter {
                color,
                density: f32v("density"),
                luminosity: boolv("luminosity"),
            }
        }
        "GradientMap" => {
            // Backward compat: pre-2026-06 files only stored `reverse` and meant a
            // black→white ramp; default the stops when absent / malformed.
            let stops = v["stops"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|s| {
                            let p = s["pos"].as_f64()? as f32;
                            let c = &s["color"];
                            let cc =
                                |i: usize| c.get(i).and_then(|x| x.as_u64()).unwrap_or(0) as u8;
                            Some((p, [cc(0), cc(1), cc(2)]))
                        })
                        .collect::<Vec<(f32, [u8; 3])>>()
                })
                .filter(|s| s.len() >= 2)
                .unwrap_or_else(|| vec![(0.0, [0, 0, 0]), (1.0, [255, 255, 255])]);
            A::GradientMap {
                stops,
                reverse: boolv("reverse"),
                dither: boolv("dither"),
            }
        }
        "Desaturate" => A::Desaturate,
        "ChannelMixer" => A::ChannelMixer {
            red: arr3("red"),
            green: arr3("green"),
            blue: arr3("blue"),
            monochrome: boolv("monochrome"),
        },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::layer::AdjustmentType as A;
    use serde_json::json;

    fn tmp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "iai-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn solid(color: [u8; 4], w: u32, h: u32) -> Canvas {
        Canvas::from_rgba(
            color
                .into_iter()
                .cycle()
                .take((w * h * 4) as usize)
                .collect(),
            w,
            h,
        )
    }

    fn minimal_pdf() -> Vec<u8> {
        b"%PDF-1.4\n1 0 obj\n<<>>\nendobj\ntrailer\n<<>>\n%%EOF\n".to_vec()
    }

    #[test]
    fn alpha_channels_round_trip_through_iai() {
        let dir = tmp_dir("alpha-channels");
        let path = dir.join("doc.iai");
        let mut canvas = solid([10, 20, 30, 255], 8, 8);
        let mut mask = vec![0u8; 64];
        mask[9] = 255;
        mask[10] = 128;
        canvas
            .channels
            .add_alpha("Cut line".into(), mask.clone(), 8, 8);
        canvas
            .channels
            .add_alpha(String::new(), vec![255u8; 64], 8, 8);

        IaiExporter
            .export(&canvas, &path, &ExportOptions::default())
            .expect("export");
        let IaiLoad::Canvas(loaded) = load(&path).expect("load") else {
            panic!("expected a plain canvas");
        };

        assert_eq!(loaded.channels.alpha.len(), 2);
        assert_eq!(loaded.channels.alpha[0].name, "Cut line");
        assert_eq!(loaded.channels.alpha[0].mask, mask);
        assert_eq!(loaded.channels.alpha[1].name, "Alpha 2");
        assert_eq!(loaded.channels.alpha[1].mask, vec![255u8; 64]);
        assert_eq!(
            (
                loaded.channels.alpha[0].width,
                loaded.channels.alpha[0].height
            ),
            (8, 8)
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn hdr_layer_round_trips_16bit_master() {
        use crate::core::tile::TileMap;
        let dir = tmp_dir("hdr-16bit");
        let path = dir.join("doc.iai");

        // Values chosen so an 8-bit round-trip cannot reproduce them: 300 is not
        // any `v*257`, and 40000 / 12345 are mid-codes the 8-bit mirror loses.
        let (w, h) = (8u32, 8u32);
        let mut px16 = vec![0u16; (w * h * 4) as usize];
        for p in 0..(w * h) as usize {
            px16[p * 4] = 300;
            px16[p * 4 + 1] = 40000;
            px16[p * 4 + 2] = 12345;
            px16[p * 4 + 3] = 65535;
        }
        let mut canvas = solid([1, 1, 1, 255], w, h);
        canvas.bit_depth = crate::core::canvas::BitDepth::Sixteen;
        canvas.layer_stack.layers[0].tiles = TileMap::from_rgba16(&px16, w, h);
        assert!(
            canvas.layer_stack.layers[0].tiles.has_hdr(),
            "test setup: layer should carry a 16-bit master"
        );

        IaiExporter
            .export(&canvas, &path, &ExportOptions::default())
            .expect("export");
        let IaiLoad::Canvas(loaded) = load(&path).expect("load") else {
            panic!("expected a plain canvas");
        };

        assert!(
            loaded.layer_stack.layers[0].tiles.has_hdr(),
            "16-bit master lost on .iai round-trip"
        );
        assert_eq!(
            loaded.layer_stack.layers[0].tiles.get_pixel16(3, 3),
            (300, 40000, 12345, 65535),
            "16-bit values not preserved bit-exact"
        );
        // The 16-bit mode flag must round-trip too, so the first edit after
        // reopening keeps preserving the masters instead of quantizing them.
        assert_eq!(
            loaded.bit_depth,
            crate::core::canvas::BitDepth::Sixteen,
            "bit_depth mode not restored on reopen"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn eight_bit_layer_stays_eight_bit() {
        // A layer without a master must NOT be promoted on save; the payload
        // stays an 8-bit PNG and reopens master-less.
        let dir = tmp_dir("ldr-8bit");
        let path = dir.join("doc.iai");
        let canvas = solid([10, 20, 30, 255], 8, 8);
        assert!(!canvas.layer_stack.layers[0].tiles.has_hdr());

        IaiExporter
            .export(&canvas, &path, &ExportOptions::default())
            .expect("export");
        let IaiLoad::Canvas(loaded) = load(&path).expect("load") else {
            panic!("expected a plain canvas");
        };
        assert!(
            !loaded.layer_stack.layers[0].tiles.has_hdr(),
            "8-bit layer gained a master it never had"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cmyk_document_round_trips_ink_and_stamps_v3() {
        let dir = tmp_dir("cmyk-v3");
        let path = dir.join("doc.iai");
        let mut canvas = solid([200, 40, 90, 255], 16, 16);
        canvas
            .convert_to_cmyk(crate::core::canvas::CmykProfile::Naive)
            .expect("convert to CMYK");

        // Ink must be the naive encoding of the original RGB.
        let want = crate::core::cms::naive_rgb_to_cmyk([200, 40, 90]);
        let mut ink = [0u8; 4];
        canvas.layer_stack.layers[0]
            .tiles
            .extract_ink_region_into(3, 3, 1, 1, &mut ink);
        assert_eq!(ink, want, "ink not encoded before save");

        IaiExporter
            .export(&canvas, &path, &ExportOptions::default())
            .expect("export");

        // The on-disk manifest must be v3 with a CMYK mode tag.
        let f = std::fs::File::open(&path).unwrap();
        let mut zip = zip::ZipArchive::new(f).unwrap();
        let manifest = read_manifest(&mut zip).unwrap();
        assert_eq!(manifest["version"].as_u64(), Some(3));
        assert_eq!(manifest["color_mode"].as_str(), Some("CMYK"));

        let IaiLoad::Canvas(loaded) = load(&path).expect("load") else {
            panic!("expected a plain canvas");
        };
        assert!(loaded.is_cmyk(), "loaded doc lost its CMYK mode");
        let mut ink2 = [0u8; 4];
        loaded.layer_stack.layers[0]
            .tiles
            .extract_ink_region_into(3, 3, 1, 1, &mut ink2);
        assert_eq!(ink2, want, "ink not restored on load");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rgb_document_stays_v2_for_old_builds() {
        let dir = tmp_dir("rgb-v2");
        let path = dir.join("doc.iai");
        let canvas = solid([10, 20, 30, 255], 8, 8);
        IaiExporter
            .export(&canvas, &path, &ExportOptions::default())
            .expect("export");
        let f = std::fs::File::open(&path).unwrap();
        let mut zip = zip::ZipArchive::new(f).unwrap();
        let manifest = read_manifest(&mut zip).unwrap();
        assert_eq!(manifest["version"].as_u64(), Some(2));
        assert!(manifest.get("color_mode").is_none_or(|v| v.is_null()));
        std::fs::remove_dir_all(&dir).ok();
    }

    // ── Path layer persistence (Bước 4 / T4.3) ──
    use crate::core::vector::affine::AffineTransform;
    use crate::core::vector::color::ColorValue;
    use crate::core::vector::object::VectorObjectData;
    use crate::core::vector::path::{Contour, FillRule, Node, PathData};
    use crate::core::vector::style::{Paint, StrokeStyle, VectorStyle};

    fn sample_path_object() -> VectorObjectData {
        use crate::core::geometry::Point;
        let path = PathData::new(
            vec![Contour::new(
                vec![
                    Node::sharp(Point::new(0.0, 0.0)),
                    Node::with_handles(
                        Point::new(40.0, 0.0),
                        Point::new(30.0, -6.0),
                        Point::new(50.0, 6.0),
                        crate::core::vector::path::NodeKind::Smooth,
                    ),
                    Node::sharp(Point::new(40.0, 40.0)),
                    Node::sharp(Point::new(0.0, 40.0)),
                ],
                true,
            )],
            FillRule::EvenOdd,
        );
        let mut style = VectorStyle::filled(ColorValue::cmyk(0.0, 0.0, 0.0, 1.0));
        style.stroke = Paint::Solid(ColorValue::rgb(0.0, 0.4, 1.0));
        style.stroke_style = StrokeStyle {
            width: 2.5,
            ..StrokeStyle::default()
        };
        style.opacity = 0.9;
        VectorObjectData::new(path, style, AffineTransform::translate(20.0, 15.0))
    }

    fn loaded_path_model(canvas: &Canvas) -> VectorObjectData {
        canvas
            .layer_stack
            .layers
            .iter()
            .find_map(|l| match &l.layer_type {
                LayerType::Path(o) => Some(o.clone()),
                _ => None,
            })
            .expect("a Path layer survived the round-trip")
    }

    #[test]
    fn path_layer_round_trips_and_stamps_v4() {
        let dir = tmp_dir("path-v4");
        let path = dir.join("doc.iai");
        let mut canvas = solid([255, 255, 255, 255], 80, 80);
        let obj = sample_path_object();
        canvas
            .execute(
                Box::new(crate::core::command_vector::CreatePathLayer::new(
                    obj.clone(),
                    "Path 1",
                )),
                crate::core::gateway::ChangeKind::LayerStructure,
            )
            .expect("create path");

        IaiExporter
            .export(&canvas, &path, &ExportOptions::default())
            .expect("export");

        // Manifest is stamped v4 because a Path layer is present.
        let f = std::fs::File::open(&path).unwrap();
        let mut zip = zip::ZipArchive::new(f).unwrap();
        assert_eq!(
            read_manifest(&mut zip).unwrap()["version"].as_u64(),
            Some(4)
        );

        let IaiLoad::Canvas(loaded) = load(&path).expect("load") else {
            panic!("expected a plain canvas");
        };
        assert_eq!(
            loaded_path_model(&loaded),
            obj,
            "geometry/style/transform must round-trip"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn path_layer_round_trips_on_cmyk() {
        let dir = tmp_dir("path-cmyk");
        let path = dir.join("doc.iai");
        let mut canvas = solid([255, 255, 255, 255], 80, 80);
        canvas
            .convert_to_cmyk(crate::core::canvas::CmykProfile::Naive)
            .expect("to CMYK");
        let obj = sample_path_object();
        canvas
            .execute(
                Box::new(crate::core::command_vector::CreatePathLayer::new(
                    obj.clone(),
                    "Path 1",
                )),
                crate::core::gateway::ChangeKind::LayerStructure,
            )
            .expect("create path");

        IaiExporter
            .export(&canvas, &path, &ExportOptions::default())
            .expect("export");
        let IaiLoad::Canvas(loaded) = load(&path).expect("load") else {
            panic!("expected a plain canvas");
        };
        assert!(loaded.is_cmyk(), "CMYK mode lost");
        // The model (incl. the CMYK fill colour) is the source of truth and must
        // survive verbatim even though the baked fallback is a mirror.
        assert_eq!(loaded_path_model(&loaded), obj);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn newer_version_is_rejected() {
        let dir = tmp_dir("newer-ver");
        let path = dir.join("doc.iai");
        // Hand-write a minimal archive claiming a version beyond this build.
        write_iai_archive(&path, |zip| {
            zip.start_file("manifest.json", stored_options())
                .map_err(|e| e.to_string())?;
            let m =
                serde_json::json!({ "version": IAI_FORMAT_VERSION + 1, "width": 4, "height": 4 });
            zip.write_all(m.to_string().as_bytes())
                .map_err(|e| e.to_string())
        })
        .expect("write");
        assert!(load(&path).is_err(), "a newer-version file must be refused");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn text_rotation_round_trips_json() {
        let mut td = crate::core::text::TextData {
            content: "Turn".to_string(),
            font_px: 32.0,
            font_family: crate::core::text::TextFontFamily::SystemFace {
                family: "Example Sans".to_string(),
                style: "SemiBold Italic".to_string(),
            },
            rotation_deg: 37.5,
            stretch_x: 1.75,
            flip_x: true,
            flip_y: true,
            ..crate::core::text::TextData::default()
        };
        td.glyph_styles = vec![
            crate::core::text::GlyphStyle {
                color: [220, 10, 20, 255],
                font_px: 32.0,
                font_family: td.font_family.clone(),
                bold: true,
                italic: false,
                underline: true,
            };
            td.content.chars().count()
        ];

        let json = text_to_json(&td);
        let restored = json_to_text_data(&json).expect("text json restores");

        assert!((restored.rotation_deg - 37.5).abs() < 0.001);
        assert_eq!(restored.font_family, td.font_family);
        assert!((restored.stretch_x - 1.75).abs() < 0.001);
        assert!(restored.flip_x);
        assert!(restored.flip_y);
        assert_eq!(restored.glyph_styles, td.glyph_styles);
    }

    #[test]
    fn shape_data_round_trips_json() {
        let (sd, _) = crate::core::shape::ShapeData::from_canvas_span(
            crate::core::shape::ShapeKind::Rectangle,
            10.0,
            20.0,
            110.0,
            80.0,
            12.0,
            true,
            [200, 30, 40, 255],
            4.0,
            [0, 0, 0, 255],
        );
        let json = shape_to_json(&sd);
        let restored = json_to_shape_data(&json).expect("shape json restores");
        assert_eq!(restored, sd);
    }

    #[test]
    fn embedded_pdf_round_trips() {
        let dir = tmp_dir("embedded-project");
        let path = dir.join("doc.iai");
        let source = dir.join("original.pdf");
        let pdf_bytes = minimal_pdf();
        let page = solid([90, 140, 210, 255], 4, 4);
        let meta = IaiProjectMeta {
            source: source.clone(),
            source_len: Some(pdf_bytes.len() as u64),
            source_modified_secs: Some(999),
            page_count: 1,
            selected_pages: vec![0],
            requested_dpi: 144.0,
            active_page: 0,
        };
        let pages = vec![IaiProjectPageOut {
            index: 0,
            base_pristine: true,
            view: (1.0, 0.0, 0.0),
            canvas: &page,
        }];

        save_pdf_project(&path, &meta, &pages, Some(&pdf_bytes)).unwrap();

        assert!(is_pdf_project(&path));
        match load(&path).unwrap() {
            IaiLoad::PdfProject(project) => {
                assert_eq!(project.source, source);
                assert_eq!(project.embedded_pdf, Some(pdf_bytes));
                assert_eq!(project.pages.len(), 1);
                assert_eq!(project.pages[0].index, 0);
            }
            _ => panic!("expected a PDF project"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn link_only_project_still_loads() {
        let dir = tmp_dir("link-only-project");
        let path = dir.join("doc.iai");
        let source = dir.join("original.pdf");
        let page = solid([30, 200, 90, 255], 5, 3);
        let meta = IaiProjectMeta {
            source: source.clone(),
            source_len: None,
            source_modified_secs: None,
            page_count: 2,
            selected_pages: vec![0, 1],
            requested_dpi: 300.0,
            active_page: 0,
        };
        let pages = vec![IaiProjectPageOut {
            index: 0,
            base_pristine: false,
            view: (1.25, 2.0, 3.0),
            canvas: &page,
        }];

        save_pdf_project(&path, &meta, &pages, None).unwrap();

        match load(&path).unwrap() {
            IaiLoad::PdfProject(project) => {
                assert_eq!(project.source, source);
                assert!(project.embedded_pdf.is_none());
                assert_eq!(project.pages.len(), 1);
                assert_eq!(
                    (
                        project.pages[0].canvas.width,
                        project.pages[0].canvas.height
                    ),
                    (5, 3)
                );
            }
            _ => panic!("expected a PDF project"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pdf_project_round_trips_metadata_and_edited_pages() {
        let dir = tmp_dir("project-roundtrip");
        let path = dir.join("doc.iai");
        let source = dir.join("original.pdf");

        let page0 = solid([200, 30, 30, 255], 4, 4);
        let page2 = solid([30, 30, 200, 255], 6, 5);
        let meta = IaiProjectMeta {
            source: source.clone(),
            source_len: Some(123),
            source_modified_secs: Some(456),
            page_count: 5,
            selected_pages: vec![0, 2, 4],
            requested_dpi: 300.0,
            active_page: 2,
        };
        let pages = vec![
            IaiProjectPageOut {
                index: 0,
                base_pristine: true,
                view: (1.5, 10.0, 20.0),
                canvas: &page0,
            },
            IaiProjectPageOut {
                index: 2,
                base_pristine: false,
                view: (2.0, -5.0, 7.0),
                canvas: &page2,
            },
        ];
        save_pdf_project(&path, &meta, &pages, None).unwrap();

        assert!(is_pdf_project(&path));

        match load(&path).unwrap() {
            IaiLoad::PdfProject(project) => {
                assert_eq!(project.source, source);
                assert_eq!(project.source_len, Some(123));
                assert_eq!(project.source_modified_secs, Some(456));
                assert!(project.embedded_pdf.is_none());
                assert_eq!(project.page_count, 5);
                assert_eq!(project.selected_pages, vec![0, 2, 4]);
                assert_eq!(project.requested_dpi, 300.0);
                assert_eq!(project.active_page, 2);
                assert_eq!(project.pages.len(), 2);

                let p0 = project.pages.iter().find(|p| p.index == 0).unwrap();
                assert!(p0.base_pristine);
                assert_eq!(p0.view, (1.5, 10.0, 20.0));
                assert_eq!((p0.canvas.width, p0.canvas.height), (4, 4));

                let p2 = project.pages.iter().find(|p| p.index == 2).unwrap();
                assert!(!p2.base_pristine);
                assert_eq!(p2.view, (2.0, -5.0, 7.0));
                assert_eq!((p2.canvas.width, p2.canvas.height), (6, 5));
                assert!(p2
                    .canvas
                    .export_flat()
                    .chunks_exact(4)
                    .all(|px| px[2] > 180 && px[0] < 60));
            }
            _ => panic!("expected a PDF project"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn single_canvas_iai_is_not_reported_as_a_project() {
        let dir = tmp_dir("single-canvas");
        let path = dir.join("image.iai");
        let canvas = solid([10, 20, 30, 255], 8, 8);
        IaiExporter
            .export(&canvas, &path, &ExportOptions::default())
            .unwrap();

        assert!(!is_pdf_project(&path));
        match load(&path).unwrap() {
            IaiLoad::Canvas(loaded) => assert_eq!((loaded.width, loaded.height), (8, 8)),
            _ => panic!("expected a single canvas"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn color_balance_empty_arrays_dont_panic() {
        let v = json!({ "type": "ColorBalance", "shadows": [], "midtones": [1.0],
                        "highlights": [0.1, 0.2, 0.3], "preserve_luminosity": true });
        let adj = json_to_adjustment(&v).expect("should parse");
        match adj {
            A::ColorBalance {
                shadows,
                midtones,
                highlights,
                ..
            } => {
                assert_eq!(shadows, [0.0, 0.0, 0.0]);
                assert_eq!(midtones, [1.0, 0.0, 0.0]);
                assert_eq!(highlights, [0.1, 0.2, 0.3]);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn curves_malformed_points_dont_panic() {
        let v = json!({ "type": "Curves", "points": [[0.0, 0.0], [], [1.0], [0.5, 0.7]] });
        let adj = json_to_adjustment(&v).expect("should parse");
        match adj {
            A::Curves { channels } => {
                assert_eq!(channels[0], vec![(0.0, 0.0), (0.5, 0.7)]);
                for ch in &channels[1..] {
                    assert_eq!(*ch, crate::core::layer::identity_curve());
                }
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn levels_legacy_json_loads_as_master_only() {
        // v≤2 files have no "r"/"g"/"b" keys — they must load with identity
        // per-channel params and the legacy fields as master.
        let v = json!({ "type": "Levels", "in_black": 10, "in_white": 200,
                        "gamma": 1.5, "out_black": 5, "out_white": 250 });
        let adj = json_to_adjustment(&v).expect("should parse");
        match adj {
            A::Levels { channels } => {
                assert_eq!(
                    (channels[0].in_black, channels[0].in_white),
                    (10, 200),
                    "legacy keys become master"
                );
                assert!((channels[0].gamma - 1.5).abs() < 1e-6);
                assert_eq!((channels[0].out_black, channels[0].out_white), (5, 250));
                for ch in &channels[1..] {
                    assert!(ch.is_identity(), "absent per-channel keys = identity");
                }
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn levels_per_channel_round_trips_and_omits_identity() {
        use crate::core::layer::LevelsParams;
        let mut channels = [LevelsParams::default(); 4];
        channels[0].gamma = 1.2;
        channels[1].in_black = 20; // red non-identity
        channels[3].out_white = 240; // blue non-identity
        let adj = A::Levels { channels };

        let j = adjustment_to_json(&adj);
        assert!(j["r"].is_object(), "non-identity red must be written");
        assert!(j["g"].is_null(), "identity green must be omitted");
        assert!(j["b"].is_object(), "non-identity blue must be written");

        let back = json_to_adjustment(&j).expect("should parse");
        assert_eq!(adj, back);
    }

    #[test]
    fn curves_per_channel_round_trips_and_omits_identity() {
        let mut channels: [Vec<(f32, f32)>; 4] =
            std::array::from_fn(|_| crate::core::layer::identity_curve());
        channels[0] = vec![(0.0, 0.0), (0.5, 0.3), (1.0, 1.0)];
        channels[2] = vec![(0.0, 0.1), (1.0, 0.9)]; // green non-identity
        let adj = A::Curves { channels };

        let j = adjustment_to_json(&adj);
        assert!(j["points_r"].is_null(), "identity red must be omitted");
        assert!(
            j["points_g"].is_array(),
            "non-identity green must be written"
        );
        assert!(j["points_b"].is_null(), "identity blue must be omitted");

        let back = json_to_adjustment(&j).expect("should parse");
        assert_eq!(adj, back);
    }

    #[test]
    fn photo_filter_short_color_doesnt_panic() {
        let v =
            json!({ "type": "PhotoFilter", "color": [200], "density": 0.25, "luminosity": true });
        let adj = json_to_adjustment(&v).expect("should parse");
        match adj {
            A::PhotoFilter { color, .. } => assert_eq!(color, [200, 0, 0]),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn gradient_map_round_trips() {
        let adj = A::GradientMap {
            stops: vec![
                (0.0, [10, 20, 30]),
                (0.5, [120, 130, 140]),
                (1.0, [240, 250, 255]),
            ],
            reverse: true,
            dither: true,
        };
        let back = json_to_adjustment(&adjustment_to_json(&adj)).expect("should parse");
        assert_eq!(adj, back);
    }

    #[test]
    fn gradient_map_legacy_reverse_only_defaults_stops() {
        // Pre-2026-06 files only stored `reverse`; loader must fill black→white.
        let v = json!({ "type": "GradientMap", "reverse": false });
        let adj = json_to_adjustment(&v).expect("should parse");
        match adj {
            A::GradientMap {
                stops,
                reverse,
                dither,
            } => {
                assert_eq!(stops, vec![(0.0, [0, 0, 0]), (1.0, [255, 255, 255])]);
                assert!(!reverse);
                assert!(!dither);
            }
            _ => panic!("wrong variant"),
        }
    }
}
